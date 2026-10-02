package ahrara

import (
	"context"
	"errors"
	"iter"
)

// RegexSource selects which complete message content to search.
type RegexSource string

const (
	// RegexText searches the decoded plain-text body, excluding attachments.
	RegexText RegexSource = "text"
	// RegexHTML searches the original decoded HTML without display sanitization.
	RegexHTML RegexSource = "html"
	// RegexSubject searches the complete decoded Subject header.
	RegexSubject RegexSource = "subject"
	// RegexRaw searches the original MIME, replacing invalid UTF-8 with U+FFFD.
	RegexRaw RegexSource = "raw"
)

// RegexOptions selects message content, expression flags and a per-call filter.
// Expressions use the shared Rust regex syntax, including named captures but
// excluding backreferences and look-around. The zero value searches plain text.
type RegexOptions struct {
	// Source defaults to RegexText.
	Source RegexSource `json:"source,omitempty"`
	// IgnoreCase enables Unicode case-insensitive matching.
	IgnoreCase bool `json:"ignore_case,omitempty"`
	// MultiLine lets ^ and $ match each line.
	MultiLine bool `json:"multi_line,omitempty"`
	// DotMatchesNewLine lets . match a newline.
	DotMatchesNewLine bool `json:"dot_matches_new_line,omitempty"`
	// Filter is applied after the inbox's admission filter and before extraction.
	Filter Filter `json:"-"`
}

// RegexGroup is one numbered capture. Index zero is the entire match.
type RegexGroup struct {
	// Index is the capture's zero-based number.
	Index int `json:"index"`
	// Name is nil for an unnamed capture.
	Name *string `json:"name"`
	// Value is nil when an optional capture did not participate.
	Value *string `json:"value"`
}

// RegexMatch includes captured values and the message's local Ahrara identifier.
type RegexMatch struct {
	// EmailID can be passed to GetEmail, RawEmail and mailbox CRUD methods.
	EmailID int64 `json:"email_id"`
	// Value is the complete matched text.
	Value string `json:"value"`
	// Groups preserves numbered, named and unmatched captures, including group zero.
	Groups []RegexGroup `json:"groups"`
}

func regexRequest(pattern string, options RegexOptions, all bool) map[string]any {
	query := struct {
		Kind    string `json:"kind"`
		Pattern string `json:"pattern"`
		All     bool   `json:"all"`
		RegexOptions
	}{Kind: "regex", Pattern: pattern, All: all, RegexOptions: options}
	request := map[string]any{"op": "search", "query": query}
	options.Filter.apply(request)
	return request
}

// WaitForRegexMatch consumes the first eligible email containing a match and
// returns its first occurrence. Nonmatching emails remain available. It has no
// default timeout; ctx controls cancellation and any deadline.
func (i *Inbox) WaitForRegexMatch(ctx context.Context, pattern string, options RegexOptions) (*RegexMatch, error) {
	result, err := i.receive(ctx, regexRequest(pattern, options, false))
	if err != nil {
		return nil, err
	}
	if len(result.Matches) == 0 {
		return nil, errors.New("unexpected Ahrara regex response")
	}

	return &result.Matches[0], nil
}

// WaitForRegexMatches yields every occurrence from successive eligible emails.
// The context deadline covers the entire iteration, including caller work, and
// yields its terminal error once. Breaking stops immediately without reading
// another email. Each selected email is consumed once, with its bounded results
// retained only while yielding that email. No goroutine or prefetch is used.
func (i *Inbox) WaitForRegexMatches(ctx context.Context, pattern string, options RegexOptions) iter.Seq2[*RegexMatch, error] {
	return func(yield func(*RegexMatch, error) bool) {
		request := regexRequest(pattern, options, true)
		for {
			result, err := i.receive(ctx, request)
			if err != nil {
				yield(nil, err)
				return
			}

			for index := range result.Matches {
				if err := ctx.Err(); err != nil {
					yield(nil, err)
					return
				}
				if !yield(&result.Matches[index], nil) {
					return
				}
			}
		}
	}
}

func xpathRequest(expression string, filter Filter, all bool) map[string]any {
	request := map[string]any{"op": "search", "query": map[string]any{"kind": "xpath", "expression": expression, "all": all}}
	filter.apply(request)
	return request
}

// WaitForXPath returns the first nonempty XPath 1.0 value from an eligible HTML
// email. HTML5 parsing repairs ordinary HTML; //a works without a namespace.
// SVG, MathML and XLink use the predefined svg, math and xlink prefixes.
// Empty strings, empty node sets and false do not consume an email.
func (i *Inbox) WaitForXPath(ctx context.Context, expression string, filter Filter) (string, error) {
	result, err := i.receive(ctx, xpathRequest(expression, filter, false))
	if err != nil {
		return "", err
	}
	if len(result.Values) == 0 {
		return "", errors.New("unexpected Ahrara XPath response")
	}

	return result.Values[0], nil
}

// WaitForXPathValues yields values in document order across all eligible emails.
// ctx covers the entire iteration; cancellation/deadline yields one final error.
// Like WaitForRegexMatches, an email is consumed when its results are selected,
// and early iteration exit leaves subsequent emails available.
func (i *Inbox) WaitForXPathValues(ctx context.Context, expression string, filter Filter) iter.Seq2[string, error] {
	return func(yield func(string, error) bool) {
		request := xpathRequest(expression, filter, true)
		for {
			result, err := i.receive(ctx, request)
			if err != nil {
				yield("", err)
				return
			}

			for _, value := range result.Values {
				if err := ctx.Err(); err != nil {
					yield("", err)
					return
				}
				if !yield(value, nil) {
					return
				}
			}
		}
	}
}
