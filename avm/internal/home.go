package internal

import (
	"os"
	"sync"
)

var (
	homeDir     string
	homeDirErr  error
	homeDirOnce sync.Once
)

// GetHomeDir returns the user's home directory, cached after the first call.
func GetHomeDir() (string, error) {
	homeDirOnce.Do(func() {
		homeDir, homeDirErr = os.UserHomeDir()
	})
	return homeDir, homeDirErr
}
