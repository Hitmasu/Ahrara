package ahrara

import (
	"context"
	"errors"
	"iter"
	"time"

	sdk "github.com/Hitmasu/Ahrara/src/sdks/go"
	"github.com/grafana/sobek"
)

// RegexOptions selects complete message content and an optional total duration.
type RegexOptions struct {
	// Source selects text (default), html, subject or raw MIME.
	Source sdk.RegexSource `js:"source"`
	// IgnoreCase enables Unicode case-insensitive matching.
	IgnoreCase bool `js:"ignoreCase"`
	// MultiLine lets ^ and $ match line boundaries.
	MultiLine bool `js:"multiLine"`
	// DotMatchesNewLine lets . match newlines.
	DotMatchesNewLine bool `js:"dotMatchesNewLine"`
	// Filter is applied after the inbox admission filter and before extraction.
	Filter sdk.Filter `js:"filter"`
	// DurationMS bounds the entire operation, including callbacks; zero uses the VU lifetime.
	DurationMS int64 `js:"durationMs"`
}

// XPathOptions selects eligible messages and an optional total duration.
type XPathOptions struct {
	// Filter selects messages after the inbox's admission filter.
	Filter sdk.Filter `js:"filter"`
	// DurationMS bounds the whole operation; zero uses the VU lifetime.
	DurationMS int64 `js:"durationMs"`
}

func (o RegexOptions) options() sdk.RegexOptions {
	return sdk.RegexOptions{
		Source:            o.Source,
		IgnoreCase:        o.IgnoreCase,
		MultiLine:         o.MultiLine,
		DotMatchesNewLine: o.DotMatchesNewLine,
		Filter:            o.Filter,
	}
}

func queryContext(ctx context.Context, durationMS int64) (context.Context, context.CancelFunc, error) {
	if durationMS < 0 || durationMS > int64((1<<63-1)/time.Millisecond) {
		return nil, nil, errors.New("durationMs must be a non-negative duration")
	}
	if durationMS == 0 {
		return ctx, func() {}, nil
	}

	ctx, cancel := context.WithTimeout(ctx, time.Duration(durationMS)*time.Millisecond)
	return ctx, cancel, nil
}

// WaitForRegexMatch returns the first match and its numbered/named capture groups.
// Its source is the complete decoded plain text unless options selects another.
func (i *Inbox) WaitForRegexMatch(pattern string, options RegexOptions) (map[string]any, error) {
	ctx, cancel, err := queryContext(i.ctx, options.DurationMS)
	if err != nil {
		return nil, err
	}

	defer cancel()
	match, err := i.inner.WaitForRegexMatch(ctx, pattern, options.options())
	if err != nil {
		return nil, err
	}

	return regexValue(match), nil
}

// WaitForXPath extracts the first nonempty XPath 1.0 value from eligible HTML.
// Empty results and false leave that message available for another receive.
func (i *Inbox) WaitForXPath(expression string, options XPathOptions) (string, error) {
	ctx, cancel, err := queryContext(i.ctx, options.DurationMS)
	if err != nil {
		return "", err
	}

	defer cancel()
	return i.inner.WaitForXPath(ctx, expression, options.Filter)
}

// ForEachRegexMatch invokes a synchronous callback for every occurrence across
// all eligible emails during one total duration. false stops; thrown errors
// propagate. No email is prefetched. Returns the number of callbacks invoked.
func (i *Inbox) ForEachRegexMatch(pattern string, callback sobek.Value, options RegexOptions) (int64, error) {
	ctx, cancel, err := queryContext(i.ctx, options.DurationMS)
	if err != nil {
		return 0, err
	}

	defer cancel()
	return forEach(i, callback, i.inner.WaitForRegexMatches(ctx, pattern, options.options()), regexValue)
}

// ForEachXPathValue invokes callback in document order across every eligible
// email. The duration includes callback work. VU cancellation and callback
// errors propagate; a completed optional duration returns the callback count.
func (i *Inbox) ForEachXPathValue(expression string, callback sobek.Value, options XPathOptions) (int64, error) {
	ctx, cancel, err := queryContext(i.ctx, options.DurationMS)
	if err != nil {
		return 0, err
	}

	defer cancel()
	return forEach(i, callback, i.inner.WaitForXPathValues(ctx, expression, options.Filter), func(value string) string { return value })
}

// forEach passes each converted value to a synchronous callback. An expired
// optional duration ends the loop normally; other errors propagate.
func forEach[T, R any](i *Inbox, callback sobek.Value, values iter.Seq2[T, error], convert func(T) R) (int64, error) {
	fn, ok := sobek.AssertFunction(callback)
	if !ok {
		return 0, errors.New("callback must be a function")
	}

	var count int64

	for value, err := range values {
		if errors.Is(err, context.DeadlineExceeded) && i.ctx.Err() == nil {
			return count, nil
		}
		if err != nil {
			return count, err
		}

		count++
		result, err := fn(sobek.Undefined(), i.vu.Runtime().ToValue(convert(value)))
		if err != nil {
			return count, err
		}
		if _, asynchronous := result.Export().(*sobek.Promise); asynchronous {
			return count, errors.New("callbacks must be synchronous")
		}
		if value, isBoolean := result.Export().(bool); isBoolean && !value {
			return count, nil
		}
	}

	return count, nil
}

func regexValue(match *sdk.RegexMatch) map[string]any {
	groups := make([]map[string]any, len(match.Groups))
	for index, group := range match.Groups {
		var name, value any
		if group.Name != nil {
			name = *group.Name
		}
		if group.Value != nil {
			value = *group.Value
		}

		groups[index] = map[string]any{"index": group.Index, "name": name, "value": value}
	}

	return map[string]any{"emailId": match.EmailID, "value": match.Value, "groups": groups}
}
