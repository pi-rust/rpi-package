import java.util.Properties
import java.util.Locale
import java.net.URI

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

val appVersion = Properties().apply {
    rootProject.file("version.properties").inputStream().use { load(it) }
}
val signingNames = listOf("KEYSTORE_PATH", "STORE_PASSWORD", "KEY_ALIAS", "KEY_PASSWORD")
val signing = signingNames.associateWith { providers.environmentVariable("RPI_WEB_ANDROID_$it").orNull }
val signingPresent = signing.values.all { !it.isNullOrBlank() }
val unsignedOption = providers.gradleProperty("allowUnsignedRelease").orNull
require(unsignedOption == null || unsignedOption in listOf("true", "false")) {
    "allowUnsignedRelease must be true or false"
}
val allowUnsignedRelease = unsignedOption == "true"
require(signing.values.none { !it.isNullOrBlank() } || signingPresent) {
    "Incomplete Android release signing environment; all four RPI_WEB_ANDROID signing variables are required"
}
require(!allowUnsignedRelease || !signingPresent) {
    "Unsigned verification must not receive release signing credentials"
}

val defaultOrigin = providers.environmentVariable("RPI_WEB_ANDROID_DEFAULT_ORIGIN").orNull?.trim().orEmpty()
val developmentOrigin = providers.environmentVariable("RPI_WEB_ANDROID_DEVELOPMENT_ORIGIN").orNull?.trim().orEmpty()
require(defaultOrigin.isNotBlank()) { "RPI_WEB_ANDROID_DEFAULT_ORIGIN is required; inject it through the build environment" }
// Do not include raw input in errors: operator addresses should not enter CI logs.
fun originLiteral(raw: String, name: String): String {
    if (raw.isEmpty()) return "\"\""
    val uri = runCatching { URI(raw) }.getOrNull()
    require(uri != null && raw.none { it <= ' ' || it == '\\' || it.code > 127 } &&
        uri.scheme.equals("https", ignoreCase = true) && uri.host != null &&
        uri.rawUserInfo == null && !uri.host.endsWith('.') && !uri.host.contains('%') &&
        (uri.port == -1 || uri.port in 1..65535) && !uri.rawAuthority.endsWith(':') &&
        uri.rawPath in listOf("", "/") && uri.rawQuery == null && uri.rawFragment == null) {
        "$name must be an HTTPS origin without credentials, path, query or fragment"
    }
    val host = uri.host.lowercase(Locale.ROOT)
    val port = if (uri.port == -1 || uri.port == 443) "" else ":${uri.port}"
    return "\"https://$host$port\""
}
val defaultOriginLiteral = originLiteral(defaultOrigin, "RPI_WEB_ANDROID_DEFAULT_ORIGIN")
val developmentOriginLiteral = originLiteral(developmentOrigin, "RPI_WEB_ANDROID_DEVELOPMENT_ORIGIN")

android {
    namespace = "cn.bigfisharc.rpiweb.local"
    compileSdk = 35
    defaultConfig {
        applicationId = "cn.bigfisharc.rpiweb.local"
        minSdk = 26
        targetSdk = 35
        versionCode = appVersion.getProperty("versionCode").toInt()
        versionName = appVersion.getProperty("versionName")
        buildConfigField("String", "DEFAULT_ORIGIN", defaultOriginLiteral)
        buildConfigField("String", "DEVELOPMENT_ORIGIN", developmentOriginLiteral)
    }
    buildFeatures { buildConfig = true }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
    signingConfigs {
        if (signingPresent) create("fixedRelease") {
            storeFile = file(signing.getValue("KEYSTORE_PATH")!!)
            storePassword = signing.getValue("STORE_PASSWORD")
            keyAlias = signing.getValue("KEY_ALIAS")
            keyPassword = signing.getValue("KEY_PASSWORD")
        }
    }
    buildTypes {
        release {
            isMinifyEnabled = false
            isDebuggable = false
            if (signingPresent) signingConfig = signingConfigs.getByName("fixedRelease")
        }
    }
    lint { abortOnError = true }
}

// PRs must explicitly request unsigned verification. Never silently fall back
// to a debug key or present an unsigned build as an installable release.
tasks.matching { it.name == "preReleaseBuild" }.configureEach {
    doFirst {
        check(signingPresent || allowUnsignedRelease) {
            "Release signing is missing. For PR verification only, pass -PallowUnsignedRelease=true"
        }
    }
}

dependencies { testImplementation("junit:junit:4.13.2") }
