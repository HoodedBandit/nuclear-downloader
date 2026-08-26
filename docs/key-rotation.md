# Updater signing-key custody and rotation

Nuclear Downloader uses one Tauri-generated signing keypair to authenticate both the application update manifest and the managed-runtime descriptor. This signature establishes updater authenticity; it is not an Authenticode signature and does not create Windows SmartScreen reputation.

## Private-key custody

Generate a keypair on a trusted offline or administratively controlled workstation with the pinned Tauri CLI. Use a long, unique password. Never paste the private key into chat, an issue, a pull request, a terminal transcript, or a shared password field.

Store only the private signing material as protected GitHub **environment secrets** on `release-candidate`:

- `TAURI_SIGNING_PRIVATE_KEY`: the complete Tauri private-key value
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`: the private key's unique password

Store the public trust set as non-secret GitHub **repository variables** so both the candidate and production environments consume the same values:

- `NUCLEAR_UPDATE_KEY_ID`: the current 1-64 character key ID
- `NUCLEAR_UPDATE_PUBLIC_KEY`: the complete current Tauri public key
- `NUCLEAR_UPDATE_NEXT_KEY_ID`: the optional second key ID during rotation
- `NUCLEAR_UPDATE_NEXT_PUBLIC_KEY`: the optional second Tauri public key during rotation

The optional pair must be set or empty together, and its ID must differ from the current ID. Do not define different environment-level variables with the same names: GitHub environment variables can shadow repository variables and cause the candidate and publish gates to use different trust anchors.

Store the exact single-line base64 contents emitted in Tauri's `.pub` file. Do not store a manually decoded Minisign block or add another encoding layer. The candidate and publish gates verify Tauri's base64 wrapper, the embedded key ID mapping, and the detached signature over each manifest's exact bytes.

Require maintainer review on the environment, restrict which branches may deploy to it, and limit repository administration. The build script reads the private key and password through Tauri's environment-variable interface. It does not pass either secret as a command-line argument or print signer output.

Keep one encrypted offline backup of the private key, password, public key, key ID, creation date, and the first release that trusted it. Use authenticated encryption on removable media kept offline, with the decryption credential stored separately. Maintain a second physical copy only if the same separation and access controls can be guaranteed. Never store plaintext key material in cloud synchronization, Git history, Actions artifacts, release assets, ordinary workstation backups, or the repository.

Perform a documented restore drill before relying on the backup. Restore into an isolated temporary location, confirm that it can sign a disposable file and that the embedded public key verifies it, then remove the temporary plaintext according to the storage medium's secure-erasure guidance. Record the drill date and result without recording secrets.

## Fail-closed response to suspected exposure

If the active private key or password may have been exposed:

1. Stop all candidate and publish workflows.
2. Remove environment access and rotate the environment reviewers or credentials implicated in the incident.
3. Preserve audit logs and identify the last unquestionably trusted release.
4. Do not publish a manifest signed only by the suspected key as though it were a routine release.
5. Prepare a reviewed recovery release and communicate the trust boundary plainly. Clients that never received a trusted next key may require a manual installer recovery path.

Deleting the GitHub secret alone does not revoke signatures already trusted by installed clients.

## Planned two-key rotation

Rotation must maintain a cryptographic chain from the key clients already trust. Never replace the embedded public key and begin signing with the replacement in the same untrusted step.

Use this sequence:

1. Generate `next` as a new Tauri keypair. Assign a new unique key ID. Back it up and protect it using the custody rules above.
2. Add the `next` public key and key ID to the updater's trusted-key set while retaining the `current` public key and key ID.
3. Build and publish a transition release signed by `current`. That old-key-signed release is what authorizes installed clients to trust both `current` and `next`.
4. Verify clean installation and upgrade from the oldest supported client to the transition release. Verify app and runtime descriptors under both key IDs in fixture tests.
5. Allow an adoption window appropriate to the installed base. Keep `current` available for rollback during that window; do not use two different keys for app and runtime metadata in one release.
6. For the first next-key-signed release, change the protected private-key secrets to `next`, make `next` the repository's current key ID/public key, and temporarily place the former `current` ID/public key in the optional second pair. Its descriptors use the new current ID while its binaries still trust both keys.
7. After the documented support window and successful upgrade evidence, clear the optional former-current ID/public key together and publish another release signed by `next` that trusts only the new current key.
8. Retire the old GitHub secret and archive or destroy the old offline private key according to the retention policy. Record the last release that accepted it.

At every step, the release that changes trusted public keys must itself be signed by a key already trusted by the clients receiving it. A public key found only in an unsigned GitHub asset, workflow variable, website, or replacement file is not a valid trust transition.

## Key identifiers and audit record

Key IDs are metadata selectors, not secrets and not cryptographic fingerprints. Use 1-64 ASCII letters, digits, `.`, `_`, or `-`; never recycle an ID for different key material, and ensure app and runtime manifests use the same active ID.

For every signed candidate, retain the private candidate inventory and acceptance record. They should identify the key ID, exact commit, exact artifact hashes, toolchain versions, candidate workflow run ID, approver, and test result. They must not contain private-key bytes, passwords, cookies, tokens, or exported user diagnostics.
