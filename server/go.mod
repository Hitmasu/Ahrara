module ahrara.dev/server

go 1.27.1

require (
	github.com/coder/websocket v1.8.14
	github.com/emersion/go-smtp v0.25.0
)

require github.com/emersion/go-sasl v0.0.0-20241020182733-b788ff22d5a6 // indirect

replace github.com/emersion/go-smtp => ./third_party/go-smtp
