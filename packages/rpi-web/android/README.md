# rpi Web Android

Android client for rpi Web, alongside [`../frontend/`](../frontend/). **This is a Kotlin Activity + system WebView shell, not a native chat UI.** It loads the selected online HTTPS node; it contains no frontend bundle, backend, credentials or model runtime. Web changes appear from the selected node without rebuilding the APK. Backend upgrades remain a separate operation.

- Android 8.0+ (API 26), target/compile API 35; Java 17.
- Application ID: `cn.bigfisharc.rpiweb.local` (retained for compatibility with the original standalone test project).
- Android version: [`version.properties`](version.properties), independent of Rust crates and Web release tags.
- Default: `https://work.bigfisharc.cn`; optional development node: `https://work-dev.bigfisharc.cn`; custom trusted **HTTPS origin only**, no path/query/credentials.
- Native Back, refresh, error retry, explicit site switching, IME/system-bar insets and system document picker. Switching sites discards the local page/draft but does not cancel server tasks. Login cookies use the app's private WebView store.
- Only `INTERNET` permission. No JavaScript bridge, broad storage permission, third-party cookies, mixed content, file URL access or TLS bypass. Main-frame TLS errors fail closed; off-origin HTTPS navigation requires confirmation and opens a system browser. App backup/transfer is excluded.
- No offline chat, app-managed downloads, background service, notifications or camera capture. Use the system document picker for existing files. Foreground Web/SSE behavior is provided by the selected node, not a new Android protocol adapter.

## Install and signing migration

Use a **signed `*-release.apk`**, verify its `.sha256`, and allow installation from the browser/file manager when Android asks. `*-unsigned-release.apk` is a CI verification output, **not installable**; `*-debug.apk` is for disposable testing and uses a different key.

The earlier `0.1.0` debug APK and the fixed-signature release use the same application ID but **different certificates**. Android will reject an in-place update. Uninstall the old debug app first, then install release. This clears that app's local login, site preference and unsent draft; it does **not** delete remote projects/sessions. Subsequent releases must retain the same release certificate and increase `versionCode`. Never rotate or lose the key casually.

Fixed release certificate SHA-256 (public; not the APK hash):

```text
e31eb4f7c12191496436f081d81eb51f8715c823c5a44c84b1d215cc1fc3cc80
```

## Local builds

Prerequisites: Java 17, Python 3.10+, Android SDK platform 35 and build-tools **35.0.0**, and network access for the first Gradle/dependency download. Set `ANDROID_SDK_ROOT` or `ANDROID_HOME`; the helper also recognizes `~/Android/Sdk`. The wrapper pins Gradle 8.14.3 and the official distribution SHA-256. Do not commit `local.properties` or machine SDK paths.

From this directory:

```sh
python tools/build.py --variant debug
python tools/build.py --variant unsigned-release
# After injecting the release environment below from a secure secret store:
python tools/build.py --variant release
# Add --offline only after dependencies are cached.
```

Windows equivalent: `./build.ps1 -Variant release [-Offline]`.

For **release only**, inject all five environment variables, not command-line passwords or `gradle.properties`:

| Variable | Purpose |
| --- | --- |
| `RPI_WEB_ANDROID_KEYSTORE_PATH` | Absolute path to the private PKCS12 keystore, outside this repository |
| `RPI_WEB_ANDROID_STORE_PASSWORD` | Keystore password |
| `RPI_WEB_ANDROID_KEY_ALIAS` | Fixed signing alias |
| `RPI_WEB_ANDROID_KEY_PASSWORD` | Key password (same as store password for PKCS12) |
| `RPI_WEB_ANDROID_CERT_SHA256` | Trusted expected certificate SHA-256, 64 hexadecimal digits |

Keep the keystore **and passwords** in access-controlled storage with a separate secure backup. Never publish either, copy them into the source tree, pass them on a command line or print the environment. Clear injected variables after the build. Debug/unsigned helper builds reject release signing variables. Gradle refuses release without signing unless `-PallowUnsignedRelease=true` is explicitly supplied; it never falls back to the debug key.

The helper runs Python guard tests, Android URL-policy unit tests, Lint and assembly, then validates the **actual APK**, not merely its filename: package/version, API bounds, INTERNET-only permissions, debuggable flag, signature and expected release certificate. Outputs are under ignored `dist/<variant>/`, with APK SHA-256 and a JSON manifest (exact source commit, dirty flag, variant, version, size and public certificate fingerprint). Logs/reports stay under ignored `artifacts/` and `app/build/`. No signing material is included in these outputs. To reverify/package an already built APK:

```sh
python tools/package_apk.py package --variant release
```

## Independent GitHub Actions

Workflow: [Build Android Web client](../../../.github/workflows/build-android.yml). It is **not** called by crates/Web release automation.

- Android/workflow/source-selector PR and main changes run unsigned verification: Python tests, URL-policy tests, debug/release Lint, debug and explicitly unsigned release assembly. These jobs have read-only permissions and no signing environment. Their artifacts are clearly labeled; unsigned release is not presented as a downloadable installable release.
- Android tag pushes run the same checks, **not signing**. A tag's mutable workflow must never be able to bypass main approval and obtain the release key.
- To produce signed artifacts, manually dispatch this workflow **from branch `main`**, with `release_tag` empty. The signed job uses the `rpi-web-android-release` environment, whose **server-side deployment branch policy must allow only branch `main`, not tags/PR refs**. Keep signing Secrets exclusively in that environment, never duplicated at repository level. Signing has no Gradle cache; the temporary keystore is removed on exit and in an `always()` cleanup step.
- To publish a GitHub Android release, increment the Android version/code in a reviewed PR, merge it, create/push the existing tag `rpi-web-android-vX.Y.Z`, then dispatch the workflow **from `main`** with that exact `release_tag`. Before any keys are read, the trusted main workflow checks that the tag resolves to a commit in main ancestry and exactly matches that commit's Android version; check and signing jobs select that source. The publish job rechecks the remote tag's commit against the built source and verifies artifact checksums.
- Only the publish job gets `contents: write`. It creates a new Android-only release with `--verify-tag --latest=false`; it does not move the repository's Latest pointer or replace existing releases/assets. A tag move or an existing release causes failure instead of clobbering.

Configure these **environment Secrets** once (via the GitHub UI or `gh secret set --env rpi-web-android-release`, stdin rather than command-line secret values):

```text
RPI_WEB_ANDROID_KEYSTORE_BASE64
RPI_WEB_ANDROID_STORE_PASSWORD
RPI_WEB_ANDROID_KEY_ALIAS
RPI_WEB_ANDROID_KEY_PASSWORD
RPI_WEB_ANDROID_CERT_SHA256
```

`KEYSTORE_BASE64` is a single-line base64 encoding of the fixed external keystore. Environment protection is a required security boundary, **not something a YAML `if` or ancestry check alone can replace**. Future maintainers must preserve this restriction and review main workflow/Gradle changes accordingly. A PR cannot verify the protected main-only signing job; first end-to-end Actions signing/publishing verification is a post-merge operator action, not an automatic PR merge or deployment.

## Verification scope

```sh
python -m unittest discover -s tools -p 'test_*.py' -v
# Install debug only on a disposable emulator, then:
npm ci
# ANDROID_SERIAL defaults to emulator-5584; physical phones are refused.
npm run test:emulator
# After replacing debug with signed release on that disposable emulator:
python tools/verify-release.py --serial emulator-5584
```

The optional Playwright dependency is **test-only**, never part of the APK. Debug smoke uses emulator CDP to intercept a local HTML fixture (no shared fixture endpoint) for real content-URI file selection/cancel/reopen, external-popup confirmation, Back history and IME layout. It also opens the real production/development login pages without supplying credentials or starting model work. The release smoke uses only ADB/UIAutomator (no CDP), checks non-debuggable installation/run-as denial and exercises real login rendering, keyboard/Back, site switching and process-restart preference persistence.

Local migration verification: Python **16/16**, URL-policy **4/4** in both variants, debug/release Lint **0 errors / 3 warnings** (`OldTargetApi`, `UnusedAttribute`, `DataExtractionRules`), debug/unsigned/signed packaging, actual missing-key/wrong-certificate rejection, debug-to-release Android signature rejection and same-signature reinstall. Android 14/API 34 x86_64 emulator with WebView 113: debug smoke **10/10**, release native smoke **6/6**. These are not physical-device, authenticated chat, real attachment-upload or model-task tests.

**Emulator limitation:** that system image has `ro.debuggable=1`. WebView 113's [`SharedStatics.setWebContentsDebuggingEnabled`](https://github.com/chromium/chromium/blob/113.0.5672.136/android_webview/glue/java/src/com/android/webview/chromium/SharedStatics.java#L48-L51) ignores the app's `false` setting on a debug Android build. The release APK is non-debuggable and calls the API with `BuildConfig.DEBUG=false`, but a devtools socket is still observed on this debug OS. Release smoke records this honestly and never connects to it; absence of CDP on a normal non-debug physical device is **not verified here**. Keep system WebView up to date.
