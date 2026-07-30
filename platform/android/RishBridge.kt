package dev.rish.runtime

/**
 * JNI-facing wrapper for the Rust JSON planning ABI.
 *
 * The JNI shim is intentionally tiny: Kotlin owns permission prompts and
 * Android APIs, while Rust owns capability negotiation and command routing.
 */
object RishBridge {
    init {
        System.loadLibrary("rish_jni")
    }

    external fun planJson(request: String): String
}
