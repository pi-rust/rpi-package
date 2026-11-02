package cn.bigfisharc.rpiweb.local

/** Local bookmarks, separate from the active connection. No Android or build configuration dependency. */
object SavedOrigins {
    const val LIMIT = 20
    const val MAX_LENGTH = 2048

    fun normalize(entries: Collection<String>): List<String> = entries.asSequence()
        .filter { it.length <= MAX_LENGTH }
        .mapNotNull(OriginPolicy::normalize)
        .distinct()
        .sorted()
        .take(LIMIT)
        .toList()

    /** Null means invalid input or no capacity. Existing entries remain valid even at capacity. */
    fun add(entries: Collection<String>, raw: String): List<String>? {
        if (raw.length > MAX_LENGTH) return null
        val selected = OriginPolicy.normalize(raw) ?: return null
        val current = normalize(entries)
        if (selected in current) return current
        if (current.size >= LIMIT) return null
        return (current + selected).sorted()
    }
}
