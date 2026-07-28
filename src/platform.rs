use crate::error::{AvmError, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Platform {
    pub os: String,
    pub arch: String,
    asset_os: String,
    asset_arch: String,
    executable_suffix: &'static str,
}

impl Platform {
    pub fn current() -> Result<Self> {
        Self::from_target(
            std::env::consts::OS,
            std::env::consts::ARCH,
            cfg!(target_endian = "little"),
        )
    }

    pub fn from_target(os: &str, arch: &str, little_endian: bool) -> Result<Self> {
        let asset_os = match os {
            "macos" => "darwin",
            "linux" => "linux",
            "windows" => "windows",
            _ => {
                return Err(AvmError::UnsupportedPlatform {
                    os: os.to_owned(),
                    arch: arch.to_owned(),
                });
            }
        };

        let asset_arch = match (os, arch, little_endian) {
            (_, "x86_64", _) => "amd64",
            ("macos" | "linux", "aarch64", _) => "arm64",
            ("linux", "powerpc64", true) => "ppc64le",
            ("linux", "s390x", _) => "s390x",
            _ => {
                return Err(AvmError::UnsupportedPlatform {
                    os: os.to_owned(),
                    arch: arch.to_owned(),
                });
            }
        };

        Ok(Self {
            os: os.to_owned(),
            arch: arch.to_owned(),
            asset_os: asset_os.to_owned(),
            asset_arch: asset_arch.to_owned(),
            executable_suffix: if os == "windows" { ".exe" } else { "" },
        })
    }

    pub fn asset_name(&self) -> String {
        format!(
            "argocd-{}-{}{}",
            self.asset_os, self.asset_arch, self.executable_suffix
        )
    }

    pub fn binary_name(&self) -> String {
        format!("argocd{}", self.executable_suffix)
    }

    pub fn avm_binary_name(&self) -> String {
        format!("avm{}", self.executable_suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_official_release_target() {
        let cases = [
            ("macos", "x86_64", true, "argocd-darwin-amd64"),
            ("macos", "aarch64", true, "argocd-darwin-arm64"),
            ("linux", "x86_64", true, "argocd-linux-amd64"),
            ("linux", "aarch64", true, "argocd-linux-arm64"),
            ("linux", "powerpc64", true, "argocd-linux-ppc64le"),
            ("linux", "s390x", false, "argocd-linux-s390x"),
            ("windows", "x86_64", true, "argocd-windows-amd64.exe"),
        ];

        for (os, arch, little_endian, expected) in cases {
            assert_eq!(
                Platform::from_target(os, arch, little_endian)
                    .unwrap()
                    .asset_name(),
                expected
            );
        }
    }

    #[test]
    fn rejects_unpublished_targets() {
        assert!(Platform::from_target("windows", "aarch64", true).is_err());
        assert!(Platform::from_target("linux", "arm", true).is_err());
        assert!(Platform::from_target("freebsd", "x86_64", true).is_err());
    }
}
