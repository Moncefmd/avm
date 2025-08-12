package internal

import (
	"runtime"
	"fmt"
)

func GetPlatform() (string, string) {
	return runtime.GOOS, runtime.GOARCH
}

func GetAssetName() string {
	os, arch := GetPlatform()
	return fmt.Sprintf("argocd-%s-%s", os, arch)
}
