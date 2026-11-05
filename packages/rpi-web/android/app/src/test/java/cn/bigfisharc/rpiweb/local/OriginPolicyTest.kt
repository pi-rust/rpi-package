package cn.bigfisharc.rpiweb.local

import org.junit.Assert.*
import org.junit.Test

class OriginPolicyTest {
    private val trusted = "https://workspace.example"

    @Test fun normalizesHttpsOrigins() {
        assertEquals(trusted, OriginPolicy.normalize(" HTTPS://WORKSPACE.EXAMPLE:443/ "))
        assertEquals("https://example.com:8443", OriginPolicy.normalize("https://example.com:8443/"))
        assertEquals("https://192.168.1.5:8443", OriginPolicy.normalize("https://192.168.1.5:8443"))
        assertEquals("https://[2001:db8::1]:8443", OriginPolicy.normalize("https://[2001:db8::1]:8443"))
    }

    @Test fun rejectsCredentialsAndNonOrigins() {
        listOf("", "http://example.com", "//example.com", "javascript:alert(1)", "data:text/html,test",
            "file:///etc/passwd", "content://documents/id", "intent://scan", "https://user:pass@example.com",
            "https://user@example.com", "https://example.com/path", "https://example.com?password=secret",
            "https://example.com/#fragment", "https://example.com:0", "https://example.com:65536",
            "https://example.com:", "https://example.com.", "https://exa mple.com", "https://éxample.com",
            "https://example.com\\@evil.com", "https://%65xample.com", "https://example.com\n.evil.com")
            .forEach { assertNull(it, OriginPolicy.normalize(it)) }
    }

    @Test fun trustsOnlyMatchingSchemeHostAndPort() {
        assertTrue(OriginPolicy.isInternal("https://workspace.example/sessions/123?a=1#entry", trusted))
        assertTrue(OriginPolicy.isInternal("https://WORKSPACE.EXAMPLE:443/", trusted))
        listOf("https://workspace.example.evil.com/", "https://evil.com/?next=https://workspace.example",
            "https://workspace.example@evil.com/", "http://workspace.example/", "https://workspace.example:444/",
            "https://development.example/", "javascript:alert(1)", "about:blank", "https://workspace.example./")
            .forEach { assertFalse(it, OriginPolicy.isInternal(it, trusted)) }
    }

    @Test fun permitsOnlyHttpsExternalLinks() {
        assertTrue(OriginPolicy.isHttps("https://docs.example.com/path?q=x#details"))
        listOf("javascript:alert(1)", "intent://a", "file:///test", "content://test", "data:text/html,test",
            "http://example.com", "https://user:pass@example.com", "https://example.com\\test")
            .forEach { assertFalse(it, OriginPolicy.isHttps(it)) }
    }

    @Test fun permitsHttpForBrowserOnlyNeverForConnection() {
        listOf("http://docs.example", "HTTP://DOCS.EXAMPLE:13031/downloads/client.apk?fixture=1#download",
            "http://192.0.2.7:13031/downloads/client.apk", "http://[2001:db8::7]:13031/client.apk")
            .forEach {
                assertTrue(it, OriginPolicy.isBrowserLink(it))
                assertFalse(it, OriginPolicy.isHttps(it))
                assertNull(it, OriginPolicy.normalize(it))
                assertFalse(it, OriginPolicy.isInternal(it, "https://docs.example"))
            }
    }

    @Test fun httpsBrowserLinksRetainExistingPolicy() {
        listOf("https://docs.example/path?q=1#details", "HTTPS://DOCS.EXAMPLE:443/", "https://docs.example:8443/client.apk")
            .forEach { assertTrue(it, OriginPolicy.isBrowserLink(it)) }
        assertTrue(OriginPolicy.isInternal("https://workspace.example/sessions/1", trusted))
        assertFalse(OriginPolicy.isInternal("http://workspace.example/sessions/1", trusted))
    }

    @Test fun browserHandoffRejectsUnsafeSchemesCredentialsAndMalformedAuthorities() {
        listOf("", "//docs.example", "javascript:alert(1)", "intent://scan", "file:///test", "content://test",
            "data:text/html,test", "mailto:user@example.com", "http://user:pass@docs.example", "https://user@docs.example",
            "http://docs.example:0", "http://docs.example:65536", "http://docs.example:", "http://docs.example.",
            "http://docs.example\\@evil.example", "http://docs.example\n.evil.example", "http://éxample.com",
            "http://[::1]suffix.example", "http://%65xample.com")
            .forEach { assertFalse(it, OriginPolicy.isBrowserLink(it)) }
    }
}
