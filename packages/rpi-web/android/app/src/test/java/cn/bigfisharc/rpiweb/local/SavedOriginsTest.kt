package cn.bigfisharc.rpiweb.local

import org.junit.Assert.*
import org.junit.Test

class SavedOriginsTest {
    @Test fun normalizesDeduplicatesAndSorts() {
        assertEquals(listOf("https://a.example", "https://b.example:8443"), SavedOrigins.normalize(listOf(
            "https://b.example:8443/", " HTTPS://A.EXAMPLE:443/ ", "https://a.example"
        )))
    }

    @Test fun filtersInvalidStoredValues() {
        assertEquals(listOf("https://valid.example"), SavedOrigins.normalize(listOf(
            "", "http://insecure.example", "https://user:password@private.example",
            "https://path.example/path", "https://query.example?key=value", "https://valid.example"
        )))
    }

    @Test fun rejectsOversizedEntries() {
        val huge = "https://" + "a".repeat(SavedOrigins.MAX_LENGTH) + ".example"
        assertTrue(SavedOrigins.normalize(listOf(huge)).isEmpty())
        assertNull(SavedOrigins.add(emptyList(), huge))
    }

    @Test fun addsWithoutMutatingSharedCollection() {
        val original = linkedSetOf("https://b.example")
        assertEquals(listOf("https://a.example", "https://b.example"), SavedOrigins.add(original, "https://a.example/"))
        assertEquals(setOf("https://b.example"), original)
    }

    @Test fun fullListAllowsExistingEntryButNeverEvictsForNewEntry() {
        val entries = (1..SavedOrigins.LIMIT).map { "https://node-$it.example" }
        assertNull(SavedOrigins.add(entries, "https://another.example"))
        assertEquals(entries.sorted(), SavedOrigins.add(entries, "HTTPS://NODE-1.EXAMPLE:443/"))
        assertEquals(SavedOrigins.LIMIT, entries.size)
    }

    @Test fun storedListIsBounded() {
        val entries = (1..100).map { "https://node-$it.example" }
        assertEquals(SavedOrigins.LIMIT, SavedOrigins.normalize(entries).size)
        assertEquals(SavedOrigins.normalize(entries), SavedOrigins.normalize(entries.reversed()))
    }

    @Test fun invalidAddDoesNotReplaceValidEntries() {
        val entries = listOf("https://valid.example")
        assertNull(SavedOrigins.add(entries, "javascript:alert(1)"))
        assertEquals(listOf("https://valid.example"), entries)
        assertEquals(entries, SavedOrigins.add(entries, "https://valid.example:443/"))
    }
}
