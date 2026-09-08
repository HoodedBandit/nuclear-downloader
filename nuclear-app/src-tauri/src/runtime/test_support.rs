use super::cache::RuntimeCacheMutation;
use super::*;

pub(crate) const FIXTURE_VERSION: &str = "2026.09.08";
pub(crate) const FIXTURE_PUBLIC_KEY: &str =
    "RWQBAgMEBQYHCAOhB7/zzhC+HXDdGOdLwJln5NYwm6UNXx3chmQSVTG4";
pub(crate) const FIXTURE_MANIFEST: &str = r#"{"schemaVersion":1,"runtimeVersion":"2026.09.08","platform":"windows-x64","tools":[{"name":"yt-dlp","version":"1.0.0","path":"yt-dlp.exe","sha256":"63e49e725c79b6301179ae8733eba81adad222a430372554653d3909d5e769f6"},{"name":"ffmpeg","version":"1.0.0","path":"ffmpeg.exe","sha256":"0d837fec8b1e45deaa1cac86d8edc3975c32f9711d1c704671b7a9484b0cae68"},{"name":"ffprobe","version":"1.0.0","path":"ffprobe.exe","sha256":"3d1e57d18a4acd9e36e9c9a7e25a62fe6839d4941888002db2b4ab8c90f35652"},{"name":"deno","version":"1.0.0","path":"deno.exe","sha256":"2e5a242ab9f68a014817e6da98ecf3b5d49e6bab7ff4f5fdb71d39d81e5c8445"}]}"#;
pub(crate) const FIXTURE_DESCRIPTOR: &str = r#"{"schemaVersion":1,"keyId":"phase4-fixture-key","runtimeVersion":"2026.09.08","platform":"windows-x64","archiveName":"nuclear-downloader-runtime-2026.09.08-windows-x64.zip","compressedSize":1,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","manifestSha256":"6459cc02abb36fe22276eb2648de2d1e4b1d26165cf36efb0d385b0203edd34b"}"#;
pub(crate) const FIXTURE_SIGNATURE: &str = "untrusted comment: phase4 fixture signature\nRUQBAgMEBQYHCCwl3gEoc4IemY9rwfDlxXZWDpFv1ulZ2o4VmliEoMTb5MgePh1gJ0T6gf44AOuIaQ8/dzxWs6pJXsddhUpmRw0=\ntrusted comment: phase4 runtime hash fixture\nOHZyIF4HbIiIuxdIgvO8uMMo1Pa87GkEnFgkynA1IzhVa16OjCzZdmboa3lfuJuDOb42VXruxHt/J6+lj8dSCA==";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VerifiedRuntimeCounts {
    pub(crate) hash_invocations: u64,
    pub(crate) hash_bytes: u64,
    pub(crate) resolution_calls: u64,
    pub(crate) successful_resolutions: u64,
}

pub(crate) struct VerifiedRuntimeHarness {
    root: PathBuf,
    cache: RuntimeCache<VerifiedRuntimeSnapshot>,
    resolution_calls: AtomicU64,
    successful_resolutions: AtomicU64,
}

pub(crate) struct VerifiedRuntimeMutation {
    root: PathBuf,
    mutation: RuntimeCacheMutation<VerifiedRuntimeSnapshot>,
}

impl VerifiedRuntimeHarness {
    pub(crate) fn create_at(explicit_root: PathBuf) -> Result<Self, String> {
        if !explicit_root.is_absolute() {
            return Err("Runtime test harness root must be absolute.".into());
        }
        ensure_no_reparse_components(&explicit_root)?;
        std::fs::create_dir(&explicit_root).map_err(|error| {
            format!(
                "Failed to create isolated runtime test harness root {}: {error}",
                explicit_root.display()
            )
        })?;
        ensure_no_reparse_components(&explicit_root)?;
        write_fixture(&explicit_root)?;
        Ok(Self {
            root: explicit_root,
            cache: RuntimeCache::new(),
            resolution_calls: AtomicU64::new(0),
            successful_resolutions: AtomicU64::new(0),
        })
    }

    pub(crate) async fn initialize(&self) -> Result<(), String> {
        initialize_runtime_cache_at(
            &self.cache,
            self.root.clone(),
            None,
            verify_fixture_signature,
            CancellationToken::new(),
        )
        .await
        .map(drop)
    }

    pub(crate) fn resolve(&self, name: &str) -> Result<Option<RuntimeToolLease>, String> {
        self.resolution_calls.fetch_add(1, Ordering::SeqCst);
        let result = resolve_tool_lease_from_cache(name, &self.cache, false);
        if matches!(&result, Ok(Some(_))) {
            self.successful_resolutions.fetch_add(1, Ordering::SeqCst);
        }
        result
    }

    pub(crate) async fn begin_mutation(&self) -> VerifiedRuntimeMutation {
        VerifiedRuntimeMutation {
            root: self.root.clone(),
            mutation: self.cache.begin_mutation().await,
        }
    }

    pub(crate) fn reset_counts(&self) {
        debug_assert!(self.root.is_absolute());
        reset_runtime_hash_counters_for_root(&self.root);
        self.resolution_calls.store(0, Ordering::SeqCst);
        self.successful_resolutions.store(0, Ordering::SeqCst);
    }

    pub(crate) fn counts(&self) -> VerifiedRuntimeCounts {
        let counts = runtime_hash_counters_for_root(&self.root);
        VerifiedRuntimeCounts {
            hash_invocations: counts.total_invocations,
            hash_bytes: counts.total_bytes,
            resolution_calls: self.resolution_calls.load(Ordering::SeqCst),
            successful_resolutions: self.successful_resolutions.load(Ordering::SeqCst),
        }
    }
}

impl VerifiedRuntimeMutation {
    pub(crate) async fn refresh(self) -> Result<(), String> {
        let Self { root, mutation } = self;
        let result = build_verified_runtime_snapshot_async(
            root,
            None,
            verify_fixture_signature,
            CancellationToken::new(),
        )
        .await;
        let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
        mutation.publish(result);
        outcome
    }
}

pub(crate) fn verify_fixture_signature(
    key_id: &str,
    bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<(), String> {
    if key_id != "phase4-fixture-key" {
        return Err("Runtime fixture used an unexpected key ID.".into());
    }
    let public_key = minisign_verify::PublicKey::from_base64(FIXTURE_PUBLIC_KEY)
        .map_err(|error| format!("Failed to parse runtime fixture public key: {error}"))?;
    let signature_text = std::str::from_utf8(signature_bytes)
        .map_err(|error| format!("Failed to parse runtime fixture signature text: {error}"))?;
    let signature = minisign_verify::Signature::decode(signature_text)
        .map_err(|error| format!("Failed to decode runtime fixture signature: {error}"))?;
    public_key
        .verify(bytes, &signature, false)
        .map_err(|error| format!("Runtime fixture signature did not verify: {error}"))
}

pub(crate) fn write_fixture(root: &Path) -> Result<PathBuf, String> {
    let runtime_dir = root.join(FIXTURE_VERSION);
    std::fs::create_dir(&runtime_dir)
        .map_err(|error| format!("Failed to create runtime fixture directory: {error}"))?;
    for name in ["yt-dlp", "ffmpeg", "ffprobe", "deno"] {
        std::fs::write(
            runtime_dir.join(format!("{name}.exe")),
            format!("{name}-phase4-fixture"),
        )
        .map_err(|error| format!("Failed to write runtime fixture tool: {error}"))?;
    }
    std::fs::write(runtime_dir.join("runtime-manifest.json"), FIXTURE_MANIFEST)
        .map_err(|error| format!("Failed to write runtime fixture manifest: {error}"))?;
    write_runtime_install_marker(&runtime_dir, FIXTURE_VERSION)?;
    std::fs::write(
        runtime_dir.join(RUNTIME_AUTH_DESCRIPTOR),
        FIXTURE_DESCRIPTOR,
    )
    .map_err(|error| format!("Failed to write runtime fixture descriptor: {error}"))?;
    std::fs::write(runtime_dir.join(RUNTIME_AUTH_SIGNATURE), FIXTURE_SIGNATURE)
        .map_err(|error| format!("Failed to write runtime fixture signature: {error}"))?;
    std::fs::write(
        root.join(RUNTIME_CURRENT_POINTER),
        serde_json::to_vec(&RuntimeCurrentPointer {
            schema_version: 1,
            runtime_version: FIXTURE_VERSION.to_string(),
        })
        .map_err(|error| format!("Failed to serialize runtime fixture pointer: {error}"))?,
    )
    .map_err(|error| format!("Failed to write runtime fixture pointer: {error}"))?;
    Ok(runtime_dir)
}
