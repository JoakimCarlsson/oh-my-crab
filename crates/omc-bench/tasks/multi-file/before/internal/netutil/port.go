// Package netutil holds small helpers for network addresses.
package netutil

import (
	"fmt"
	"strconv"
	"strings"
)

// ParsePort reads text as a TCP port from 1 to 65535.
func ParsePort(text string) (int, error) {
	port, err := strconv.Atoi(strings.TrimSpace(text))
	if err != nil {
		return 0, fmt.Errorf("not a number: %q", text)
	}
	if port < 1 || port > 65535 {
		return 0, fmt.Errorf("out of range: %d", port)
	}
	return port, nil
}

// ParsePortRange reads text as two ports with a dash between them, the lower first.
func ParsePortRange(text string) (int, int, error) {
	low, high, found := strings.Cut(text, "-")
	if !found {
		return 0, 0, fmt.Errorf("not a range: %q", text)
	}
	from, err := ParsePort(low)
	if err != nil {
		return 0, 0, err
	}
	to, err := ParsePort(high)
	if err != nil {
		return 0, 0, err
	}
	if from > to {
		return 0, 0, fmt.Errorf("backwards range: %q", text)
	}
	return from, to, nil
}
