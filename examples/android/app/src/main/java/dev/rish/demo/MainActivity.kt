package dev.rish.demo

import android.app.Activity
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.os.Build
import android.os.Bundle
import android.text.Spannable
import android.text.SpannableStringBuilder
import android.text.style.ForegroundColorSpan
import android.text.style.StyleSpan
import android.util.Log
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import dev.rish.runtime.RishAppletConfiguration
import dev.rish.runtime.RishBridge
import dev.rish.runtime.RishGuestCommand
import org.json.JSONArray
import org.json.JSONObject
import kotlin.concurrent.thread

class MainActivity : Activity() {
    private val pageBackground = Color.parseColor("#07111F")
    private val cardBackground = Color.parseColor("#101C2D")
    private val accent = Color.parseColor("#5EEAD4")
    private val success = Color.parseColor("#7EE787")
    private val secondaryText = Color.parseColor("#94A3B8")
    private val primaryText = Color.parseColor("#F1F5F9")
    private val border = Color.parseColor("#25354A")
    private val terminalBlack = Color.parseColor("#09131F")
    private val errorColor = Color.parseColor("#FDA4AF")

    private lateinit var abiPill: TextView
    private lateinit var statusPill: TextView
    private lateinit var terminalOutput: TextView

    @Suppress("DEPRECATION")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.statusBarColor = pageBackground
        window.navigationBarColor = pageBackground
        setContentView(buildDashboard())

        thread(name = "rish-demo") {
            runCatching { runDemo() }
                .onSuccess { result ->
                    val report = result.logReport()
                    Log.i(TAG, "\n$report")
                    runOnUiThread { renderSuccess(result) }
                }
                .onFailure { error ->
                    Log.e(TAG, "demo failed", error)
                    runOnUiThread { renderFailure(error) }
                }
        }
    }

    private fun buildDashboard(): View {
        val page = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(20), dp(24), dp(20), dp(40))
        }

        page.addView(buildBrandHeader())
        page.addView(space(24))
        page.addView(
            label(
                "PORTABLE COMMAND LAYER",
                11f,
                accent,
                Typeface.create("sans-serif-medium", Typeface.NORMAL),
                letterSpacing = 0.18f,
            ),
        )
        page.addView(space(10))
        page.addView(
            label(
                "Linux tools, native on mobile.",
                34f,
                primaryText,
                Typeface.create("sans-serif", Typeface.BOLD),
            ).apply {
                setLineSpacing(dp(2).toFloat(), 1f)
            },
        )
        page.addView(space(12))
        page.addView(
            label(
                "Rust command semantics, dispatched through a verified Android sandbox bridge.",
                15f,
                secondaryText,
            ).apply {
                setLineSpacing(dp(4).toFloat(), 1f)
            },
        )
        page.addView(space(24))
        page.addView(buildPlatformPills())
        page.addView(space(24))
        page.addView(buildTerminalCard())
        page.addView(space(18))
        page.addView(buildBoundaryCard())
        page.addView(space(22))
        page.addView(
            label(
                "RUST CORE  /  JNI BRIDGE  /  ANDROID HOST",
                10f,
                secondaryText,
                Typeface.create("sans-serif-medium", Typeface.NORMAL),
                letterSpacing = 0.12f,
            ).apply {
                gravity = Gravity.CENTER
            },
        )

        return ScrollView(this).apply {
            isFillViewport = true
            isVerticalScrollBarEnabled = false
            overScrollMode = View.OVER_SCROLL_NEVER
            setBackgroundColor(pageBackground)
            addView(
                page,
                ViewGroup.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT,
                    ViewGroup.LayoutParams.WRAP_CONTENT,
                ),
            )
        }
    }

    private fun buildBrandHeader(): View {
        val mark = label(
            ">_",
            17f,
            pageBackground,
            Typeface.MONOSPACE,
        ).apply {
            gravity = Gravity.CENTER
            background = rounded(accent, 13)
        }

        val wordmark = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_VERTICAL
            addView(
                label(
                    "rish",
                    28f,
                    primaryText,
                    Typeface.create("sans-serif", Typeface.BOLD),
                ),
            )
            addView(
                label(
                    "MOBILE LINUX RUNTIME",
                    9f,
                    secondaryText,
                    Typeface.create("sans-serif-medium", Typeface.NORMAL),
                    letterSpacing = 0.16f,
                ),
            )
        }

        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(mark, LinearLayout.LayoutParams(dp(46), dp(46)))
            addView(
                wordmark,
                LinearLayout.LayoutParams(
                    0,
                    ViewGroup.LayoutParams.WRAP_CONTENT,
                    1f,
                ).apply {
                    marginStart = dp(13)
                },
            )
            addView(statusDot())
            addView(
                label(
                    "ONLINE",
                    10f,
                    success,
                    Typeface.create("sans-serif-medium", Typeface.NORMAL),
                    letterSpacing = 0.12f,
                ),
                LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.WRAP_CONTENT,
                    ViewGroup.LayoutParams.WRAP_CONTENT,
                ).apply {
                    marginStart = dp(7)
                },
            )
        }
    }

    private fun buildPlatformPills(): View {
        val row = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            addView(platformPill("ANDROID ${Build.VERSION.RELEASE}"))
            addView(platformPill(architectureLabel()), pillMargins())
            abiPill = platformPill("ABI …")
            addView(abiPill)
        }
        return HorizontalScrollView(this).apply {
            isHorizontalScrollBarEnabled = false
            overScrollMode = View.OVER_SCROLL_NEVER
            addView(
                row,
                ViewGroup.LayoutParams(
                    ViewGroup.LayoutParams.WRAP_CONTENT,
                    ViewGroup.LayoutParams.WRAP_CONTENT,
                ),
            )
        }
    }

    private fun buildTerminalCard(): View {
        val trafficLights = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(terminalDot(Color.parseColor("#FB7185")))
            addView(terminalDot(Color.parseColor("#FBBF24")), dotMargins())
            addView(terminalDot(success))
        }
        statusPill = label(
            "RUNNING",
            9f,
            accent,
            Typeface.create("sans-serif-medium", Typeface.NORMAL),
            letterSpacing = 0.12f,
        ).apply {
            gravity = Gravity.CENTER
            setPadding(dp(11), dp(5), dp(11), dp(5))
            background = rounded(Color.parseColor("#142A31"), 20, accent)
        }
        val header = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(17), dp(14), dp(14), dp(14))
            addView(trafficLights)
            addView(
                label(
                    "RISH SESSION",
                    10f,
                    secondaryText,
                    Typeface.create("sans-serif-medium", Typeface.NORMAL),
                    letterSpacing = 0.14f,
                ),
                LinearLayout.LayoutParams(
                    0,
                    ViewGroup.LayoutParams.WRAP_CONTENT,
                    1f,
                ).apply {
                    marginStart = dp(13)
                },
            )
            addView(statusPill)
        }

        terminalOutput = label(
            "",
            13f,
            primaryText,
            Typeface.MONOSPACE,
        ).apply {
            setPadding(dp(18), dp(18), dp(18), dp(20))
            setLineSpacing(dp(3).toFloat(), 1f)
            setTextIsSelectable(true)
            text = initialTerminalText()
        }

        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            background = rounded(cardBackground, 18, border)
            elevation = dp(8).toFloat()
            addView(header)
            addView(
                View(this@MainActivity).apply {
                    setBackgroundColor(border)
                },
                LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT,
                    dp(1),
                ),
            )
            addView(
                LinearLayout(this@MainActivity).apply {
                    setBackgroundColor(terminalBlack)
                    addView(
                        terminalOutput,
                        LinearLayout.LayoutParams(
                            ViewGroup.LayoutParams.MATCH_PARENT,
                            ViewGroup.LayoutParams.WRAP_CONTENT,
                        ),
                    )
                },
                LinearLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT,
                    ViewGroup.LayoutParams.WRAP_CONTENT,
                ),
            )
        }
    }

    private fun buildBoundaryCard(): View {
        val icon = label(
            "◇",
            22f,
            accent,
            Typeface.create("sans-serif", Typeface.BOLD),
        ).apply {
            gravity = Gravity.CENTER
            background = rounded(Color.parseColor("#142A31"), 13, accent)
        }
        val copy = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(
                label(
                    "CAPABILITY BOUNDARY",
                    10f,
                    accent,
                    Typeface.create("sans-serif-medium", Typeface.NORMAL),
                    letterSpacing = 0.14f,
                ),
            )
            addView(space(7))
            addView(
                label(
                    "Portable applets · App sandbox",
                    16f,
                    primaryText,
                    Typeface.create("sans-serif-medium", Typeface.NORMAL),
                ),
            )
            addView(space(5))
            addView(
                label(
                    "Explicit semantics. No Android kernel privilege claims.",
                    12f,
                    secondaryText,
                ),
            )
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setPadding(dp(16), dp(17), dp(16), dp(17))
            background = rounded(cardBackground, 17, border)
            addView(icon, LinearLayout.LayoutParams(dp(46), dp(46)))
            addView(
                copy,
                LinearLayout.LayoutParams(
                    0,
                    ViewGroup.LayoutParams.WRAP_CONTENT,
                    1f,
                ).apply {
                    marginStart = dp(14)
                },
            )
        }
    }

    private fun runDemo(): DemoResult {
        val abi = RishBridge.protocolVersion()
        check(abi == 1) { "unexpected ABI version" }

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

        return DemoResult(
            abi = abi,
            grepPlan = "${grepPlan.getString("kind")}/${grepPlan.getString("name")}",
            echo = echo.trimEnd(),
            sha256 = sha256.trimEnd(),
        )
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

    private fun renderSuccess(result: DemoResult) {
        abiPill.text = "ABI v${result.abi}"
        statusPill.text = "PASS"
        statusPill.setTextColor(success)
        statusPill.background = rounded(Color.parseColor("#162A27"), 20, success)

        terminalOutput.text = SpannableStringBuilder().apply {
            appendStyled("●  PASS", success, bold = true)
            appendStyled("   verified portable session", secondaryText)
            append("\n\n")
            appendStyled("\$ plan grep needle\n", accent)
            appendStyled("  ↳ ${result.grepPlan}\n\n", primaryText)
            appendStyled("\$ echo \"hello from Android\"\n", accent)
            appendStyled("${result.echo}\n\n", primaryText)
            appendStyled("\$ printf abc | sha256sum\n", accent)
            appendStyled("${result.sha256}\n\n", primaryText)
            appendStyled("exit 0", success, bold = true)
        }
    }

    private fun renderFailure(error: Throwable) {
        abiPill.text = "ABI ERROR"
        statusPill.text = "FAIL"
        statusPill.setTextColor(errorColor)
        statusPill.background = rounded(Color.parseColor("#351B27"), 20, errorColor)
        terminalOutput.text = SpannableStringBuilder().apply {
            appendStyled("●  FAIL\n\n", errorColor, bold = true)
            appendStyled("\$ rish demo\n", accent)
            appendStyled(
                "${error::class.java.simpleName}: ${error.message ?: "unknown error"}",
                primaryText,
            )
        }
    }

    private fun initialTerminalText(): CharSequence =
        SpannableStringBuilder().apply {
            appendStyled("●  RUNNING", accent, bold = true)
            appendStyled("   negotiating ABI\n\n", secondaryText)
            appendStyled("\$ initialize rish bridge", accent)
        }

    private fun label(
        value: CharSequence,
        textSize: Float,
        color: Int,
        font: Typeface = Typeface.create("sans-serif", Typeface.NORMAL),
        letterSpacing: Float = 0f,
    ): TextView =
        TextView(this).apply {
            text = value
            this.textSize = textSize
            setTextColor(color)
            typeface = font
            includeFontPadding = false
            this.letterSpacing = letterSpacing
        }

    private fun platformPill(value: String): TextView =
        label(
            value,
            10f,
            accent,
            Typeface.create("sans-serif-medium", Typeface.NORMAL),
            letterSpacing = 0.1f,
        ).apply {
            gravity = Gravity.CENTER
            setPadding(dp(13), dp(8), dp(13), dp(8))
            background = rounded(Color.parseColor("#102630"), 24, Color.parseColor("#2F756F"))
        }

    private fun statusDot(): View =
        View(this).apply {
            background = GradientDrawable().apply {
                shape = GradientDrawable.OVAL
                setColor(success)
            }
            layoutParams = LinearLayout.LayoutParams(dp(7), dp(7))
        }

    private fun terminalDot(color: Int): View =
        View(this).apply {
            background = GradientDrawable().apply {
                shape = GradientDrawable.OVAL
                setColor(color)
            }
            layoutParams = LinearLayout.LayoutParams(dp(9), dp(9))
        }

    private fun rounded(
        fill: Int,
        radius: Int,
        stroke: Int? = null,
    ): GradientDrawable =
        GradientDrawable().apply {
            shape = GradientDrawable.RECTANGLE
            cornerRadius = dp(radius).toFloat()
            setColor(fill)
            stroke?.let { setStroke(dp(1), it) }
        }

    private fun space(height: Int): View =
        View(this).apply {
            layoutParams = LinearLayout.LayoutParams(dp(1), dp(height))
        }

    private fun pillMargins(): LinearLayout.LayoutParams =
        LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.WRAP_CONTENT,
            ViewGroup.LayoutParams.WRAP_CONTENT,
        ).apply {
            marginStart = dp(8)
            marginEnd = dp(8)
        }

    private fun dotMargins(): LinearLayout.LayoutParams =
        LinearLayout.LayoutParams(dp(9), dp(9)).apply {
            marginStart = dp(7)
            marginEnd = dp(7)
        }

    private fun architectureLabel(): String =
        if (Build.SUPPORTED_ABIS.any { it.startsWith("arm64") }) {
            "ARM64"
        } else {
            Build.SUPPORTED_ABIS.firstOrNull()?.uppercase() ?: "UNKNOWN ABI"
        }

    private fun dp(value: Int): Int =
        (value * resources.displayMetrics.density).toInt()

    private fun SpannableStringBuilder.appendStyled(
        value: String,
        color: Int,
        bold: Boolean = false,
    ) {
        val start = length
        append(value)
        setSpan(
            ForegroundColorSpan(color),
            start,
            length,
            Spannable.SPAN_EXCLUSIVE_EXCLUSIVE,
        )
        if (bold) {
            setSpan(
                StyleSpan(Typeface.BOLD),
                start,
                length,
                Spannable.SPAN_EXCLUSIVE_EXCLUSIVE,
            )
        }
    }

    private data class DemoResult(
        val abi: Int,
        val grepPlan: String,
        val echo: String,
        val sha256: String,
    ) {
        fun logReport(): String =
            buildString {
                appendLine("PASS — rish Android demo")
                appendLine("ABI: $abi")
                appendLine("grep plan: $grepPlan")
                appendLine("echo: $echo")
                append("sha256sum: $sha256")
            }
    }

    companion object {
        private const val TAG = "RishAndroidDemo"
    }
}
