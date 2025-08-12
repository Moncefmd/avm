package internal

import (
	"fmt"
	"runtime"
	"testing"
)

func TestGetPlatform(t *testing.T) {
	os, arch := GetPlatform()
	if os != runtime.GOOS {
		t.Errorf("expected os %s, got %s", runtime.GOOS, os)
	}
	if arch != runtime.GOARCH {
		t.Errorf("expected arch %s, got %s", runtime.GOARCH, arch)
	}
}

func TestGetAssetName(t *testing.T) {
	assetName := GetAssetName()
	expectedName := fmt.Sprintf("argocd-%s-%s", runtime.GOOS, runtime.GOARCH)
	if assetName != expectedName {
		t.Errorf("expected asset name %s, got %s", expectedName, assetName)
	}
}
