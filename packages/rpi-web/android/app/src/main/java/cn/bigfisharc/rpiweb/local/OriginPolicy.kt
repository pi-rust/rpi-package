package cn.bigfisharc.rpiweb.local

import java.net.URI
import java.util.Locale

/** No browser/Android dependency: all trust comparisons use scheme/host/port, not prefixes. */
object OriginPolicy {
    private fun uri(raw: String, allowHttp: Boolean = false): URI? = try {
        if (raw.isBlank() || raw.any { it <= ' ' || it == '\\' || it.code > 127 }) null
        else URI(raw).takeIf {
            (it.scheme.equals("https", ignoreCase = true) ||
                (allowHttp && it.scheme.equals("http", ignoreCase = true))) &&
                it.rawUserInfo == null && it.host != null &&
                !it.host.endsWith('.') && !it.host.contains('%') &&
                (it.port == -1 || it.port in 1..65535) &&
                !it.rawAuthority.endsWith(':')
        }
    } catch (_: Exception) { null }

    private fun origin(uri: URI): String = "https://${uri.host.lowercase(Locale.ROOT)}" +
        if (uri.port == -1 || uri.port == 443) "" else ":${uri.port}"

    /** Settings accept origins only: no silently ignored path, credentials, query or fragment. */
    fun normalize(raw: String): String? {
        val parsed = uri(raw.trim()) ?: return null
        if (parsed.rawPath !in listOf("", "/") || parsed.rawQuery != null || parsed.rawFragment != null) return null
        return origin(parsed)
    }

    fun isHttps(raw: String): Boolean = uri(raw) != null

    /** Browser handoff only: HTTP is never a trusted connection origin. */
    fun isBrowserLink(raw: String): Boolean = uri(raw, allowHttp = true) != null

    fun isInternal(raw: String, trustedOrigin: String): Boolean {
        val parsed = uri(raw) ?: return false
        return origin(parsed) == trustedOrigin
    }
}
