package cn.bigfisharc.rpiweb.local

/** Generated at build time from environment only; never fall back to an operator's address. */
object ConfiguredOrigins {
    val default: String = checkNotNull(OriginPolicy.normalize(BuildConfig.DEFAULT_ORIGIN)) {
        "Invalid generated default origin"
    }
    val development: String? = BuildConfig.DEVELOPMENT_ORIGIN.takeIf { it.isNotBlank() }?.let {
        checkNotNull(OriginPolicy.normalize(it)) { "Invalid generated development origin" }
    }
}
