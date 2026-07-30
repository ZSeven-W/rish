package dev.rish.demo

import android.app.Activity
import android.os.Bundle
import android.util.Log
import android.widget.ScrollView
import android.widget.TextView
import dev.rish.runtime.RishAppletConfiguration
import dev.rish.runtime.RishBridge
import dev.rish.runtime.RishGuestCommand
import org.json.JSONArray
import org.json.JSONObject
import kotlin.concurrent.thread

class MainActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val output = TextView(this).apply {
            text = "Running Rust/JNI demo…"
            textSize = 16f
            typeface = android.graphics.Typeface.MONOSPACE
            setPadding(32, 32, 32, 32)
        }
        setContentView(ScrollView(this).apply { addView(output) })

        thread(name = "rish-demo") {
            val report = runCatching { runDemo() }
                .getOrElse { error ->
                    Log.e(TAG, "demo failed", error)
                    "FAIL: ${error::class.java.simpleName}: ${error.message}"
                }
            Log.i(TAG, "\n$report")
            runOnUiThread { output.text = report }
        }
    }

    private fun runDemo(): String {
        check(RishBridge.protocolVersion() == 1) { "unexpected ABI version" }

        val grepRequest = JSONObject()
            .put("platform", "android")
            .put("privilege", "app_sandbox")
            .put(
                "command",
                JSONObject()
                    .put("program", "grep")
                    .put("args", JSONArray().put("needle"))
                    .put("cwd", "/"),
            )
            .toString()
        val grepResponse = JSONObject(RishBridge.planJson(grepRequest))
        check(grepResponse.getBoolean("ok")) { grepResponse.optString("error") }
        val grepPlan = grepResponse.getJSONObject("plan")
        check(grepPlan.getString("kind") == "portable_applet")
        check(grepPlan.getString("name") == "grep")

        val configuration = RishAppletConfiguration.inAppFiles(
            applicationContext,
            directoryName = "demo-sandbox",
            user = "android-demo",
            hostname = "rish-android",
        )
        val echo = execute(
            RishGuestCommand(
                program = "echo",
                args = listOf("hello from Android"),
            ),
            configuration,
        )
        check(echo == "hello from Android\n") { "unexpected echo output: $echo" }

        val sha256 = execute(
            RishGuestCommand(
                program = "sha256sum",
                stdin = "abc".encodeToByteArray(),
            ),
            configuration,
        )
        val expectedHash =
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  -\n"
        check(sha256 == expectedHash) { "unexpected sha256sum output: $sha256" }

        return buildString {
            appendLine("PASS — rish Android demo")
            appendLine("ABI: ${RishBridge.protocolVersion()}")
            appendLine("grep plan: ${grepPlan.getString("kind")}/${grepPlan.getString("name")}")
            append("echo: $echo")
            append("sha256sum: $sha256")
        }.trimEnd()
    }

    private fun execute(
        command: RishGuestCommand,
        configuration: RishAppletConfiguration,
    ): String {
        val response = JSONObject(
            RishBridge.executePortableApplet(command, configuration),
        )
        check(response.getBoolean("ok")) { response.optString("error") }
        val outcome = response.getJSONObject("outcome")
        check(outcome.getInt("exit_code") == 0) {
            "applet failed: ${decode(outcome.getJSONArray("stderr"))}"
        }
        val path = outcome.getJSONObject("path")
        check(path.getString("kind") == "portable_applet")
        check(path.getString("name") == command.program)
        return decode(outcome.getJSONArray("stdout"))
    }

    private fun decode(bytes: JSONArray): String {
        val output = ByteArray(bytes.length()) { index ->
            val value = bytes.getInt(index)
            check(value in 0..255) { "invalid output byte" }
            value.toByte()
        }
        return output.toString(Charsets.UTF_8)
    }

    companion object {
        private const val TAG = "RishAndroidDemo"
    }
}
