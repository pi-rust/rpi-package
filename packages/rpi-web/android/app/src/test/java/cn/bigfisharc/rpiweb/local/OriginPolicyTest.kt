package cn.bigfisharc.rpiweb.local

import org.junit.Assert.*
import org.junit.Test

class OriginPolicyTest {
    @Test fun normalizesHttpsOrigins() {
        assertEquals(OriginPolicy.PRODUCTION, OriginPolicy.normalize(" HTTPS://WORK.BIGFISHARC.CN:443/ "))
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
        assertTrue(OriginPolicy.isInternal("https://work.bigfisharc.cn/sessions/123?a=1#entry", OriginPolicy.PRODUCTION))
        assertTrue(OriginPolicy.isInternal("https://WORK.bigfisharc.cn:443/", OriginPolicy.PRODUCTION))
        listOf("https://work.bigfisharc.cn.evil.com/", "https://evil.com/?next=https://work.bigfisharc.cn",
            "https://work.bigfisharc.cn@evil.com/", "http://work.bigfisharc.cn/", "https://work.bigfisharc.cn:444/",
            "https://work-dev.bigfisharc.cn/", "javascript:alert(1)", "about:blank", "https://work.bigfisharc.cn./")
            .forEach { assertFalse(it, OriginPolicy.isInternal(it, OriginPolicy.PRODUCTION)) }
    }

    @Test fun permitsOnlyHttpsExternalLinks() {
        assertTrue(OriginPolicy.isHttps("https://docs.example.com/path?q=x#details"))
        listOf("javascript:alert(1)", "intent://a", "file:///test", "content://test", "data:text/html,test",
            "http://example.com", "https://user:pass@example.com", "https://example.com\\test")
            .forEach { assertFalse(it, OriginPolicy.isHttps(it)) }
    }
}
