package internal

import (
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
)

func TestDownloadFile(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		w.Write([]byte("test content"))
	}))
	defer server.Close()

	tempDir := t.TempDir()
	filePath := filepath.Join(tempDir, "downloaded_file")

	err := DownloadFile(server.URL, filePath)
	if err != nil {
		t.Fatalf("DownloadFile returned an error: %v", err)
	}

	content, err := os.ReadFile(filePath)
	if err != nil {
		t.Fatalf("could not read downloaded file: %v", err)
	}

	if string(content) != "test content" {
		t.Errorf("expected content 'test content', got '%s'", string(content))
	}
}
