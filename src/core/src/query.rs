//! Regex and XPath extraction shared by the embedded SDKs.
use anyhow::{Result, anyhow, bail};
use html5ever_xpath::tendril::TendrilSink;
use mailparse::MailHeaderMap;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use sxd_document::{Package, QName, dom};
use sxd_xpath::{Context, Factory, Value};

const MAX_RESULTS: usize = 10_000;
const MAX_RESULT_BYTES: usize = 16 * 1024 * 1024;
const MAX_DOM_NODES: usize = 100_000;
const MAX_DOM_DEPTH: usize = 256;

/// The decoded message content searched by a regular expression.
#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegexSource {
    #[default]
    Text,
    Html,
    Subject,
    Raw,
}

/// A query evaluated against each eligible message before consuming its receipt.
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Query {
    Regex {
        pattern: String,
        #[serde(default)]
        source: RegexSource,
        #[serde(default)]
        ignore_case: bool,
        #[serde(default)]
        multi_line: bool,
        #[serde(default)]
        dot_matches_new_line: bool,
        #[serde(default)]
        all: bool,
    },
    #[serde(rename = "xpath")]
    XPath {
        expression: String,
        #[serde(default)]
        all: bool,
    },
}

impl Query {
    /// A plain-text regex with default flags.
    pub(crate) fn regex(pattern: impl Into<String>, all: bool) -> Self {
        Self::Regex {
            pattern: pattern.into(),
            source: Default::default(),
            ignore_case: false,
            multi_line: false,
            dot_matches_new_line: false,
            all,
        }
    }
}

/// A numbered capture; group zero is the entire match.
#[derive(Debug, Deserialize, Serialize)]
pub struct RegexGroup {
    pub index: usize,
    pub name: Option<String>,
    pub value: Option<String>,
}

/// An owned match tied to its Ahrara message identifier.
#[derive(Debug, Deserialize, Serialize)]
pub struct RegexMatch {
    pub email_id: i64,
    pub value: String,
    pub groups: Vec<RegexGroup>,
}

/// Results from one email. Empty queries leave that email unconsumed.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct QueryMatches {
    pub matches: Vec<RegexMatch>,
    pub values: Vec<String>,
}

/// A validated query safe to retain across asynchronous receive operations.
pub struct CompiledQuery(Compiled);

enum Compiled {
    Regex {
        regex: Regex,
        source: RegexSource,
        all: bool,
    },
    XPath {
        expression: String,
        all: bool,
    },
}

impl CompiledQuery {
    /// Validates syntax before waiting for or consuming any email.
    pub fn compile(query: Query) -> Result<Self> {
        Ok(Self(match query {
            Query::Regex {
                pattern,
                source,
                ignore_case,
                multi_line,
                dot_matches_new_line,
                all,
            } => {
                validate_expression(&pattern)?;
                let regex = RegexBuilder::new(&pattern)
                    .case_insensitive(ignore_case)
                    .multi_line(multi_line)
                    .dot_matches_new_line(dot_matches_new_line)
                    .build()
                    .map_err(|_| anyhow!("invalid_argument: invalid regular expression"))?;

                Compiled::Regex { regex, source, all }
            }
            Query::XPath { expression, all } => {
                compile_xpath(&expression)?;
                Compiled::XPath { expression, all }
            }
        }))
    }

    /// Extracts results without mutating receipt state. No match leaves the
    /// message available for a different query. Results are bounded to 10,000
    /// entries and 16 MiB of strings; HTML to 100,000 nodes and 256 levels.
    pub fn evaluate(
        &self,
        metadata: &crate::storage::Email,
        raw: &[u8],
    ) -> Result<Option<QueryMatches>> {
        if raw.len() > crate::protocol::MAX_EMAIL {
            bail!("invalid_message: message exceeds 10 MiB")
        }

        let mut result = QueryMatches::default();
        let mut bytes = 0;
        match &self.0 {
            Compiled::Regex { regex, source, all } => {
                let content = match source {
                    RegexSource::Raw => String::from_utf8_lossy(raw).into_owned(),
                    RegexSource::Subject => parse(raw)?
                        .headers
                        .get_first_value("Subject")
                        .unwrap_or_default(),
                    RegexSource::Text => match bodies(raw)?.0 {
                        Some(body) => body,
                        None => return Ok(None),
                    },
                    RegexSource::Html => match bodies(raw)?.1 {
                        Some(body) => body,
                        None => return Ok(None),
                    },
                };

                for captures in regex.captures_iter(&content) {
                    if result.matches.len() == MAX_RESULTS {
                        bail!("resource_limit: query exceeds 10000 results")
                    }

                    let value = captures
                        .get(0)
                        .expect("regex always includes group zero")
                        .as_str();
                    account(&mut bytes, value.len())?;
                    let mut groups = Vec::with_capacity(captures.len());

                    for (index, name) in regex.capture_names().enumerate() {
                        let value = captures.get(index).map(|value| value.as_str());
                        // Count group storage too, so empty captures cannot evade the bound.
                        account(
                            &mut bytes,
                            size_of::<RegexGroup>()
                                + name.map_or(0, str::len)
                                + value.map_or(0, str::len),
                        )?;

                        groups.push(RegexGroup {
                            index,
                            name: name.map(str::to_owned),
                            value: value.map(str::to_owned),
                        });
                    }

                    result.matches.push(RegexMatch {
                        email_id: metadata.id,
                        value: value.to_owned(),
                        groups,
                    });

                    if !all {
                        break;
                    }
                }
            }
            Compiled::XPath { expression, all } => {
                let Some(html) = bodies(raw)?.1 else {
                    return Ok(None);
                };
                let package = html_document(&html)?;
                let mut context = Context::new();
                context.set_namespace("svg", "http://www.w3.org/2000/svg");
                context.set_namespace("math", "http://www.w3.org/1998/Math/MathML");
                context.set_namespace("xlink", "http://www.w3.org/1999/xlink");
                // sxd's compiled expression is !Send. Validate before the wait,
                // then compile locally for evaluation instead of an unsafe wrapper.
                let xpath = compile_xpath(expression)?;
                let value = xpath
                    .evaluate(&context, package.as_document().root())
                    .map_err(|_| anyhow!("invalid_argument: XPath evaluation failed"))?;

                match value {
                    Value::Nodeset(nodes) => {
                        for node in nodes.document_order() {
                            let value = node.string_value();
                            if value.is_empty() {
                                continue;
                            }
                            if result.values.len() == MAX_RESULTS {
                                bail!("resource_limit: query exceeds 10000 results")
                            }

                            account(&mut bytes, value.len())?;
                            result.values.push(value);
                            if !all {
                                break;
                            }
                        }
                    }
                    Value::Boolean(false) => {}
                    value => {
                        let value = value.string();
                        if !value.is_empty() {
                            account(&mut bytes, value.len())?;
                            result.values.push(value);
                        }
                    }
                }
            }
        }

        Ok((!result.matches.is_empty() || !result.values.is_empty()).then_some(result))
    }
}

fn parse(raw: &[u8]) -> Result<mailparse::ParsedMail<'_>> {
    mailparse::parse_mail(raw).map_err(|_| anyhow!("invalid_message: invalid MIME message"))
}

fn bodies(raw: &[u8]) -> Result<(Option<String>, Option<String>)> {
    let mut text = None;
    let mut html = None;
    crate::mail::message_bodies(&parse(raw)?, &mut text, &mut html)?;
    Ok((text, html))
}

fn compile_xpath(expression: &str) -> Result<sxd_xpath::XPath> {
    validate_expression(expression)?;
    // sxd recursively parses and evaluates its AST. A byte limit alone still
    // permits enough nested groups to overflow a normal native worker stack.
    // This scan only bounds complexity; Factory remains the XPath parser.
    let (mut quote, mut depth, mut tokens, mut word) = (None, 0usize, 0usize, false);

    for character in expression.chars() {
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            }

            continue;
        }
        if character.is_whitespace() {
            word = false;
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            tokens += 1;
            word = false;
        } else if character.is_alphanumeric() || matches!(character, '_' | ':' | '.') {
            if !word {
                tokens += 1;
            }

            word = true;
        } else {
            tokens += 1;
            word = false;
            match character {
                '(' | '[' => depth += 1,
                ')' | ']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }

        if depth > 32 || tokens > 256 {
            bail!("invalid_argument: XPath exceeds 32 nested groups or 256 syntax tokens")
        }
    }

    Factory::new()
        .build(expression)
        .map_err(|_| anyhow!("invalid_argument: invalid XPath expression"))?
        .ok_or_else(|| anyhow!("invalid_argument: empty XPath expression"))
}

fn validate_expression(expression: &str) -> Result<()> {
    if expression.is_empty() || expression.len() > 4096 {
        bail!("invalid_argument: query expressions must contain 1..4096 bytes")
    }

    Ok(())
}

fn account(bytes: &mut usize, size: usize) -> Result<()> {
    *bytes = bytes.saturating_add(size);
    if *bytes > MAX_RESULT_BYTES {
        bail!("resource_limit: query exceeds 16 MiB result budget")
    }

    Ok(())
}

/// HTML5 repairs malformed markup before it becomes an XPath document. HTML
/// elements have no namespace so ordinary //a queries work; SVG/MathML retain
/// theirs and use the predefined svg/math prefixes.
fn html_document(html: &str) -> Result<Package> {
    let parsed = html5ever_xpath::parse_document(RcDom::default(), Default::default()).one(html);
    let package = Package::new();
    let document = package.as_document();
    let mut pending: Vec<(Handle, Option<dom::Element<'_>>, usize)> =
        vec![(parsed.document.clone(), None, 0)];
    let mut nodes = 0;

    while let Some((node, parent, depth)) = pending.pop() {
        nodes += 1;
        if nodes > MAX_DOM_NODES || depth > MAX_DOM_DEPTH {
            bail!("resource_limit: HTML exceeds XPath document bounds")
        }

        let parent = match &node.data {
            NodeData::Document => parent,
            NodeData::Element { name, attrs, .. } => {
                let namespace = name.ns.as_ref();
                let name = if namespace == "http://www.w3.org/1999/xhtml" {
                    QName::new(name.local.as_ref())
                } else {
                    QName::with_namespace_uri(Some(namespace), name.local.as_ref())
                };
                let element = document.create_element(name);

                for attribute in attrs.borrow().iter() {
                    let namespace = attribute.name.ns.as_ref();
                    element.set_attribute_value(
                        QName::with_namespace_uri(
                            (!namespace.is_empty()).then_some(namespace),
                            attribute.name.local.as_ref(),
                        ),
                        attribute.value.as_ref(),
                    );
                }
                if let Some(parent) = parent {
                    parent.append_child(element);
                } else {
                    document.root().append_child(element);
                }

                Some(element)
            }
            NodeData::Text { contents } => {
                if let Some(parent) = parent {
                    parent.append_child(document.create_text(contents.borrow().as_ref()));
                }

                parent
            }
            NodeData::Comment { contents } => {
                let comment = document.create_comment(contents.as_ref());
                if let Some(parent) = parent {
                    parent.append_child(comment);
                } else {
                    document.root().append_child(comment);
                }

                parent
            }
            _ => parent,
        };

        pending.extend(
            node.children
                .borrow()
                .iter()
                .rev()
                .map(|child| (child.clone(), parent, depth + 1)),
        );
    }

    Ok(package)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn evaluate(query: serde_json::Value, body: &str, html: bool) -> Result<Option<QueryMatches>> {
        let raw = format!(
            "Subject: =?UTF-8?Q?Ol=C3=A1_ABC-12?=\r\nContent-Type: text/{}; charset=utf-8\r\n\r\n{body}",
            if html { "html" } else { "plain" }
        );
        let metadata = crate::storage::Email {
            id: 7,
            transfer_id: String::new(),
            identity_fingerprint: String::new(),
            recipient: "recipient@example.test".into(),
            envelope_sender: "sender@example.test".into(),
            received_at: "2026-10-02T12:00:00Z".into(),
            from_header: String::new(),
            to_header: String::new(),
            subject: "stored preview".into(),
            message_id: None,
            size_bytes: raw.len() as i64,
        };

        CompiledQuery::compile(serde_json::from_value(query)?)?.evaluate(&metadata, raw.as_bytes())
    }

    #[test]
    fn regex_preserves_all_matches_named_and_absent_groups() {
        let result = evaluate(
            json!({
                "kind": "regex",
                "pattern": "(?P<name>[a-z]+)(?:-(\\d+))?",
                "ignore_case": true,
                "all": true
            }),
            "ABC-12 def",
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.matches.len(), 2);
        assert_eq!(result.matches[0].email_id, 7);
        assert_eq!(result.matches[0].groups[0].value.as_deref(), Some("ABC-12"));
        assert_eq!(result.matches[0].groups[1].name.as_deref(), Some("name"));
        assert_eq!(result.matches[1].groups[2].value, None);
        assert!(result.values.is_empty());
    }

    #[test]
    fn regex_sources_use_full_decoded_bodies_and_default_to_first() {
        let body = format!("{}\nABC-12\nABC-34", "x".repeat(280 * 1024));
        let result = evaluate(
            json!({
                "kind": "regex",
                "pattern": "^abc-[0-9]+$",
                "ignore_case": true,
                "multi_line": true
            }),
            &body,
            false,
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].value, "ABC-12");
        let result = evaluate(
            json!({
                "kind": "regex",
                "pattern": "Olá",
                "source": "subject"
            }),
            "",
            false,
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.matches[0].value, "Olá");
        assert!(
            evaluate(
                json!({"kind":"regex","pattern":"ABC","source":"html"}),
                "ABC",
                false
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn xpath_repairs_html_and_returns_nodes_in_document_order() {
        let html = "<!doctype html><title>Hi &amp; bye</title><p><a href='/first?a=1&amp;b=2'>First<br>line</a><p><a href='/second'>Second</a>";
        let result = evaluate(
            json!({
                "kind": "xpath",
                "expression": "//a/@href",
                "all": true
            }),
            html,
            true,
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.values, ["/first?a=1&b=2", "/second"]);
        let result = evaluate(
            json!({
                    "kind": "xpath",
                    "expression": "//a[starts-with(@href,'/first')]/ancestor::p/following-sibling::p/a/text()"
                }),
            html,
            true
        ).unwrap().unwrap();

        assert_eq!(result.values, ["Second"]);
    }

    #[test]
    fn xpath_scalar_and_empty_values_follow_xpath_semantics() {
        let html = "<p> A <b>B</b> </p><p>C</p>";
        for (expression, expected) in [
            ("normalize-space(//p)", "A B"),
            ("count(//p)", "2"),
            ("boolean(//b)", "true"),
        ] {
            let result = evaluate(
                json!({"kind": "xpath", "expression": expression}),
                html,
                true,
            )
            .unwrap()
            .unwrap();

            assert_eq!(result.values, [expected]);
        }

        for expression in ["//missing", "string(//missing)", "boolean(//missing)"] {
            assert!(
                evaluate(json!({"kind":"xpath","expression":expression}), html, true)
                    .unwrap()
                    .is_none()
            );
        }

        assert!(
            evaluate(
                json!({"kind":"xpath","expression":"count(//p)"}),
                html,
                false
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn invalid_queries_fail_before_reading_an_email_without_echoing_input() {
        for query in [
            json!({"kind": "regex", "pattern": "[secret"}),
            json!({"kind": "xpath", "expression": "//*[secret"}),
            json!({"kind": "xpath", "expression": ""}),
        ] {
            let error = CompiledQuery::compile(serde_json::from_value(query).unwrap())
                .err()
                .unwrap()
                .to_string();
            assert!(error.starts_with("invalid_argument:"));
            assert!(!error.contains("secret"));
        }

        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<CompiledQuery>();
    }

    #[test]
    fn query_results_and_html_structure_have_explicit_bounds() {
        let error = evaluate(
            json!({"kind": "regex", "pattern": "a", "all": true}),
            &"a".repeat(MAX_RESULTS + 1),
            false,
        )
        .unwrap_err();
        assert!(error.to_string().starts_with("resource_limit:"));
        let error = evaluate(
            json!({"kind": "regex", "pattern": "(a+)"}),
            &"a".repeat(6 * 1024 * 1024),
            false,
        )
        .unwrap_err();

        assert!(error.to_string().starts_with("resource_limit:"));
        let html = format!(
            "{}text{}",
            "<div>".repeat(MAX_DOM_DEPTH),
            "</div>".repeat(MAX_DOM_DEPTH)
        );
        let error =
            evaluate(json!({"kind": "xpath", "expression": "//div"}), &html, true).unwrap_err();

        assert!(error.to_string().starts_with("resource_limit:"));
    }

    #[test]
    fn xpath_keeps_svg_namespaces_and_regex_reads_raw_mime() {
        let html = "<svg><a href='drawing'>Label</a></svg><a href='html'>Other</a>";
        let result = evaluate(
            json!({
                "kind": "xpath",
                "expression": "//svg:a/@href"
            }),
            html,
            true,
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.values, ["drawing"]);
        let result = evaluate(
            json!({"kind": "xpath", "expression": "//a/@href"}),
            html,
            true,
        )
        .unwrap()
        .unwrap();

        assert_eq!(result.values, ["html"]);
        let result = evaluate(
            json!({
                "kind": "regex",
                "pattern": "Subject:.*\\r\\nContent-Type:",
                "source": "raw"
            }),
            html,
            true,
        )
        .unwrap()
        .unwrap();

        assert!(result.matches[0].value.starts_with("Subject:"));
    }

    #[test]
    fn query_configuration_is_bounded_and_missing_body_cannot_match() {
        for query in [
            json!({"kind": "regex", "pattern": ""}),
            json!({"kind": "regex", "pattern": "a".repeat(4097)}),
            json!({
                "kind": "xpath",
                "expression": " ".repeat(4097)
            }),
        ] {
            let result = CompiledQuery::compile(serde_json::from_value(query).unwrap());
            assert!(
                result
                    .err()
                    .is_some_and(|error| error.to_string().starts_with("invalid_argument:"))
            );
        }

        assert!(
            evaluate(
                json!({"kind":"regex","pattern":".*","source":"html"}),
                "plain",
                false
            )
            .unwrap()
            .is_none()
        );

        assert!(
            evaluate(
                json!({"kind":"regex","pattern":".*","source":"text"}),
                "<p>html</p>",
                true
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn xpath_complexity_is_bounded_without_counting_quoted_markup() {
        for expression in [
            format!("{}1{}", "(".repeat(33), ")".repeat(33)),
            vec!["1"; 200].join("+"),
        ] {
            let error =
                compile_xpath(&expression).expect_err("unsafe XPath complexity must be rejected");
            assert!(error.to_string().starts_with("invalid_argument:"));
        }

        let expression = format!("string('{}')", "([+-])".repeat(500));

        assert!(compile_xpath(&expression).is_ok());
    }
}
