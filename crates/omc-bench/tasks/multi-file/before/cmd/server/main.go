// Command server listens on the configured port.
package main

import (
	"fmt"
	"os"

	"example.com/server/internal/config"
	"example.com/server/internal/netutil"
)

// main starts the server, or prints why it cannot.
func main() {
	settings, err := config.Load()
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	if len(os.Args) > 1 {
		port, err := netutil.ParsePort(os.Args[1])
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		settings.Port = port
	}
	fmt.Printf("listening on %s:%d\n", settings.Host, settings.Port)
}
