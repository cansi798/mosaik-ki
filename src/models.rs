use crate::Tier;

/// Eine herunterzuladende Datei aus Hugging Face, geprüft per SHA-256.
#[derive(Debug, PartialEq, Eq)]
pub struct ModelFile {
    pub repo: &'static str,
    pub file: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

impl ModelFile {
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/main/{}",
            self.repo, self.file
        )
    }
}

pub const KOMPAKT_MODEL: ModelFile = ModelFile {
    repo: "google/gemma-4-E2B-it-qat-q4_0-gguf",
    file: "gemma-4-E2B_q4_0-it.gguf",
    sha256: "fa401b55b07ee70a54c6dae3903c783a6e65064312529ea57175cb5f8dec6634",
    size: 3_349_516_256,
};

pub const PRO_MODEL: ModelFile = ModelFile {
    repo: "google/gemma-4-E4B-it-qat-q4_0-gguf",
    file: "gemma-4-E4B_q4_0-it.gguf",
    sha256: "676c35070db6dbe52f93e9c864ee0fba4eddea94b9c875d9cb10daff453fbaee",
    size: 5_154_941_280,
};

pub const PRO_MMPROJ: ModelFile = ModelFile {
    repo: "google/gemma-4-E4B-it-qat-q4_0-gguf",
    file: "gemma-4-E4B-it-mmproj.gguf",
    sha256: "7498a37cb619e55f2fcf87eb931f56e99389ed6d432e4c5c66110694c0d65578",
    size: 991_552_256,
};

static KOMPAKT_FILES: [ModelFile; 1] = [KOMPAKT_MODEL];
static PRO_FILES: [ModelFile; 2] = [PRO_MODEL, PRO_MMPROJ];

pub fn model_files(tier: Tier) -> &'static [ModelFile] {
    match tier {
        Tier::Standard => &[],
        Tier::Kompakt => &KOMPAKT_FILES,
        Tier::Pro => &PRO_FILES,
    }
}

pub fn main_model(tier: Tier) -> Option<&'static ModelFile> {
    model_files(tier).first()
}

pub fn mmproj(tier: Tier) -> Option<&'static ModelFile> {
    model_files(tier).get(1)
}

/// llama.cpp-Version, deren `llama-server` mitgeliefert wird.
pub const LLAMA_BUILD: &str = "b11718";

#[derive(Debug, PartialEq, Eq)]
pub struct LlamaAsset {
    pub name: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

impl LlamaAsset {
    pub fn url(&self) -> String {
        format!(
            "https://github.com/ggml-org/llama.cpp/releases/download/{LLAMA_BUILD}/{}",
            self.name
        )
    }
}

static LINUX_X64: LlamaAsset = LlamaAsset {
    name: "llama-b11718-bin-ubuntu-x64.tar.gz",
    sha256: "99f83b15cea9a4fb8945516c276b5f42b11608f82104fc6fa98a541cac826de7",
    size: 17_968_484,
};
static WINDOWS_X64: LlamaAsset = LlamaAsset {
    name: "llama-b11718-bin-win-cpu-x64.zip",
    sha256: "92cc529b29a43f9a30e21d490cb2e95062acd0b59af6fee8b3b52b28074f838c",
    size: 19_662_572,
};
static MACOS_ARM64: LlamaAsset = LlamaAsset {
    name: "llama-b11718-bin-macos-arm64.tar.gz",
    sha256: "a6f2574e4996d74f93bdcc32e2139539b276e14edfba3388cda5a5e9ebbc445a",
    size: 12_292_559,
};
static MACOS_X64: LlamaAsset = LlamaAsset {
    name: "llama-b11718-bin-macos-x64.tar.gz",
    sha256: "ddcb33aa91b875da0d2b2d909282232a78aa0511e615aac8ab435bc802d29dd5",
    size: 11_781_266,
};

/// `os`/`arch` wie `std::env::consts::OS`/`ARCH`. `None` = KI auf dieser Plattform nicht verfügbar.
pub fn llama_asset(os: &str, arch: &str) -> Option<&'static LlamaAsset> {
    match (os, arch) {
        ("linux", "x86_64") => Some(&LINUX_X64),
        ("windows", "x86_64") => Some(&WINDOWS_X64),
        ("macos", "aarch64") => Some(&MACOS_ARM64),
        ("macos", "x86_64") => Some(&MACOS_X64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_downloads_nothing_pro_downloads_two_files() {
        assert!(model_files(Tier::Standard).is_empty());
        assert_eq!(model_files(Tier::Kompakt), &[KOMPAKT_MODEL]);
        assert_eq!(main_model(Tier::Pro), Some(&PRO_MODEL));
        assert_eq!(mmproj(Tier::Pro), Some(&PRO_MMPROJ));
        assert_eq!(mmproj(Tier::Kompakt), None);
    }

    #[test]
    fn urls_point_to_official_sources() {
        assert_eq!(
            KOMPAKT_MODEL.url(),
            "https://huggingface.co/google/gemma-4-E2B-it-qat-q4_0-gguf/resolve/main/gemma-4-E2B_q4_0-it.gguf"
        );
        assert_eq!(
            PRO_MODEL.url(),
            "https://huggingface.co/google/gemma-4-E4B-it-qat-q4_0-gguf/resolve/main/gemma-4-E4B_q4_0-it.gguf"
        );
        let a = llama_asset("linux", "x86_64").unwrap();
        assert!(
            a.url()
                .starts_with("https://github.com/ggml-org/llama.cpp/releases/download/b11718/")
        );
        assert_eq!(llama_asset("linux", "riscv64"), None);
    }

    #[test]
    fn checksums_are_64_hex_chars() {
        let all = [&KOMPAKT_MODEL, &PRO_MODEL, &PRO_MMPROJ];
        for f in all {
            assert_eq!(f.sha256.len(), 64);
            assert!(f.sha256.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }
}
