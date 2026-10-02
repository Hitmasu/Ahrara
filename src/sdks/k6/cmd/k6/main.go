// A source-checkout equivalent of an xk6 build with k6/x/ahrara registered.
package main

import (
	_ "github.com/Hitmasu/Ahrara/src/sdks/k6"
	"go.k6.io/k6/v2/cmd"
)

func main() { cmd.Execute() }
