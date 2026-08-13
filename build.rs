use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=SLURM_LOG_TEST_BUILD");
    println!("cargo:rerun-if-env-changed=SLURM_LOG_TEST_RELEASE_PUBLIC_KEY");
    println!("cargo:rustc-check-cfg=cfg(slurm_log_test_build)");
    if env::var("SLURM_LOG_TEST_BUILD").as_deref() == Ok("1") {
        let key = env::var("SLURM_LOG_TEST_RELEASE_PUBLIC_KEY")
            .expect("SLURM_LOG_TEST_RELEASE_PUBLIC_KEY is required with SLURM_LOG_TEST_BUILD=1");
        let valid = key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit());
        assert!(
            valid,
            "SLURM_LOG_TEST_RELEASE_PUBLIC_KEY must be a 32-byte Ed25519 public key encoded as 64 hexadecimal characters"
        );
        println!("cargo:rustc-cfg=slurm_log_test_build");
        println!("cargo:rustc-env=SLURM_LOG_TEST_RELEASE_PUBLIC_KEY={key}");
    }
}
