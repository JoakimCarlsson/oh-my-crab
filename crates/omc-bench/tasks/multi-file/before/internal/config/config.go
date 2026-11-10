// Package config reads the server's settings from the environment.
package config

import (
	"os"

	"example.com/server/internal/netutil"
)

// Config is everything the server is told when it starts.
type Config struct {
	Host string
	Port int
	Low  int
	High int
}

// Load reads the settings, falling back to port 8080 and the range 9000-9100.
func Load() (Config, error) {
	config := Config{Host: os.Getenv("HOST"), Port: 8080, Low: 9000, High: 9100}
	if text := os.Getenv("PORT"); text != "" {
		port, err := netutil.ParsePort(text)
		if err != nil {
			return Config{}, err
		}
		config.Port = port
	}
	if text := os.Getenv("PORT_RANGE"); text != "" {
		low, high, err := netutil.ParsePortRange(text)
		if err != nil {
			return Config{}, err
		}
		config.Low, config.High = low, high
	}
	return config, nil
}
