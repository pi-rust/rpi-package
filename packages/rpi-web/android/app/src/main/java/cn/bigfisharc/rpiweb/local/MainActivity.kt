package cn.bigfisharc.rpiweb.local

import android.annotation.SuppressLint
import android.app.Activity
import android.app.AlertDialog
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.res.Configuration
import android.content.res.ColorStateList
import android.graphics.Bitmap
import android.graphics.Color
import android.net.Uri
import android.net.http.SslError
import android.os.Build
import android.os.Bundle
import android.os.Message
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.webkit.CookieManager
import android.webkit.MimeTypeMap
import android.webkit.PermissionRequest
import android.webkit.SslErrorHandler
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.TextView
import android.widget.Toast
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher

class MainActivity : Activity() {
    private lateinit var root: LinearLayout
    private lateinit var content: FrameLayout
    private lateinit var progress: ProgressBar
    private lateinit var address: TextView
    private lateinit var errorPanel: LinearLayout
    private lateinit var errorText: TextView
    private var web: WebView? = null
    private var origin = OriginPolicy.PRODUCTION
    private var fileCallback: ValueCallback<Array<Uri>>? = null
    private var chooserLaunched = false
    private val popups = mutableSetOf<WebView>()
    private val preferences by lazy { getSharedPreferences("connection", MODE_PRIVATE) }
    private var backCallback: OnBackInvokedCallback? = null
    private val dialogs = mutableSetOf<AlertDialog>()
    private val bg get() = getColor(R.color.shell_background)
    private val textColor get() = getColor(R.color.shell_text)

    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        origin = OriginPolicy.normalize(preferences.getString("origin", null) ?: "") ?: OriginPolicy.PRODUCTION
        buildLayout()
        applySystemTheme()
        createWebView(state?.takeIf { it.getString("origin") == origin }?.getBundle("web"))
        if (Build.VERSION.SDK_INT >= 33) {
            backCallback = OnBackInvokedCallback { navigateBack() }.also {
                onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, it)
            }
        }
    }

    private fun dp(value: Int) = (value * resources.displayMetrics.density).toInt()

    private fun button(label: String, description: String, action: () -> Unit) = Button(this).apply {
        text = label
        contentDescription = description
        isAllCaps = false
        textSize = 14f
        minWidth = dp(48)
        minimumWidth = dp(48)
        setPadding(dp(8), 0, dp(8), 0)
        val ripple = android.util.TypedValue()
        theme.resolveAttribute(android.R.attr.selectableItemBackground, ripple, true)
        setBackgroundResource(ripple.resourceId)
        setTextColor(textColor)
        setOnClickListener { action() }
    }

    private fun buildLayout() {
        root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(bg)
            fitsSystemWindows = true
        }
        val toolbar = LinearLayout(this).apply { gravity = Gravity.CENTER_VERTICAL }
        toolbar.addView(button("‹", "返回", ::navigateBack), LinearLayout.LayoutParams(dp(48), dp(48)))
        address = TextView(this).apply {
            text = Uri.parse(origin).host
            gravity = Gravity.CENTER_VERTICAL
            textSize = 13f
            setSingleLine()
            ellipsize = android.text.TextUtils.TruncateAt.MIDDLE
            setPadding(dp(4), 0, dp(4), 0)
            contentDescription = "当前站点：$origin"
        }
        toolbar.addView(address, LinearLayout.LayoutParams(0, dp(48), 1f))
        toolbar.addView(button("刷新", "刷新页面") { reload() }, LinearLayout.LayoutParams(dp(58), dp(48)))
        toolbar.addView(button("站点", "站点设置", ::showSites), LinearLayout.LayoutParams(dp(58), dp(48)))
        root.addView(toolbar)
        progress = ProgressBar(this, null, android.R.attr.progressBarStyleHorizontal).apply {
            max = 100
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
        }
        root.addView(progress, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(2)))
        content = FrameLayout(this)
        root.addView(content, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        errorPanel = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER
            setPadding(dp(24), dp(24), dp(24), dp(24))
            setBackgroundColor(bg)
            visibility = View.GONE
        }
        errorText = TextView(this).apply { textSize = 16f; gravity = Gravity.CENTER }
        errorPanel.addView(errorText)
        errorPanel.addView(button("重试", "重试加载页面") { reload() })
        errorPanel.addView(button("更换站点", "更换连接站点", ::showSites))
        setContentView(root)
        // Target 35 enforces edge-to-edge. Keep both native toolbar and composer
        // outside system bars/IME; do not inject viewport or keyboard JavaScript.
        root.setOnApplyWindowInsetsListener { view, insets ->
            if (Build.VERSION.SDK_INT >= 30) {
                val bars = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
                val ime = insets.getInsets(WindowInsets.Type.ime())
                view.setPadding(bars.left, bars.top, bars.right, maxOf(bars.bottom, ime.bottom))
                WindowInsets.CONSUMED
            } else {
                @Suppress("DEPRECATION")
                view.setPadding(insets.systemWindowInsetLeft, insets.systemWindowInsetTop,
                    insets.systemWindowInsetRight, insets.systemWindowInsetBottom)
                @Suppress("DEPRECATION")
                insets.consumeSystemWindowInsets()
            }
        }
        root.requestApplyInsets()
    }

    @SuppressLint("SetJavaScriptEnabled")
    private fun createWebView(saved: Bundle? = null) {
        cancelFileCallback()
        clearPopups()
        web?.let { old ->
            content.removeView(old)
            old.stopLoading()
            old.webChromeClient = null
            old.destroy()
        }
        web = WebView(this).apply {
            setBackgroundColor(getColor(R.color.page_background))
            settings.apply {
                javaScriptEnabled = true
                domStorageEnabled = true
                allowFileAccess = false
                allowContentAccess = true // System picker content URIs, not arbitrary filesystem URLs.
                mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
                javaScriptCanOpenWindowsAutomatically = false
                setSupportMultipleWindows(true)
                mediaPlaybackRequiresUserGesture = true
                safeBrowsingEnabled = true
            }
            CookieManager.getInstance().setAcceptCookie(true)
            CookieManager.getInstance().setAcceptThirdPartyCookies(this, false)
            webViewClient = client()
            webChromeClient = chrome()
            setDownloadListener { _, _, _, _, _ ->
                Toast.makeText(this@MainActivity, "本版不提供应用内下载，请在浏览器中打开站点下载。", Toast.LENGTH_LONG).show()
            }
        }
        WebView.setWebContentsDebuggingEnabled(BuildConfig.DEBUG)
        content.removeAllViews()
        content.addView(web, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        content.addView(errorPanel, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        errorPanel.visibility = View.GONE
        address.text = Uri.parse(origin).host
        address.contentDescription = "当前站点：$origin"
        val restored = saved?.let { web?.restoreState(it) }
        if (restored == null || !OriginPolicy.isInternal(web?.url ?: "", origin)) web?.loadUrl("$origin/")
    }

    private fun client() = object : WebViewClient() {
        override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
            val url = request.url.toString()
            if (OriginPolicy.isInternal(url, origin)) return false
            if (request.isForMainFrame && request.hasGesture() && OriginPolicy.isHttps(url)) confirmExternal(url)
            return true
        }

        override fun onPageStarted(view: WebView, url: String, favicon: Bitmap?) {
            if (view !== web) return
            if (!OriginPolicy.isInternal(url, origin)) {
                view.stopLoading()
                showError("已阻止非当前站点的页面跳转。请通过站点设置更换连接。")
                return
            }
            errorPanel.visibility = View.GONE
            progress.visibility = View.VISIBLE
        }

        override fun onPageFinished(view: WebView, url: String) {
            if (view !== web) return
            progress.visibility = View.INVISIBLE
            CookieManager.getInstance().flush()
        }

        override fun onReceivedError(view: WebView, request: WebResourceRequest, error: WebResourceError) {
            if (view === web && request.isForMainFrame) showError("页面加载失败（${error.errorCode}）。\n请检查网络、站点地址后重试。")
        }

        override fun onReceivedHttpError(view: WebView, request: WebResourceRequest, response: WebResourceResponse) {
            if (view === web && request.isForMainFrame && response.statusCode >= 400)
                showError("站点暂不可用（HTTP ${response.statusCode}）。\n可以重试或更换站点。")
        }

        override fun onReceivedSslError(view: WebView, handler: SslErrorHandler, error: SslError) {
            handler.cancel() // Never bypass TLS, even for custom nodes.
            if (view === web) showError("站点证书校验失败，已停止连接。\n请检查站点证书和手机时间。")
        }
    }

    private fun chrome() = object : WebChromeClient() {
        override fun onProgressChanged(view: WebView, value: Int) {
            if (view !== web) return
            progress.progress = value
            progress.visibility = if (value < 100 && errorPanel.visibility != View.VISIBLE) View.VISIBLE else View.INVISIBLE
        }

        override fun onPermissionRequest(request: PermissionRequest) { request.deny() }

        override fun onCreateWindow(view: WebView, isDialog: Boolean, isUserGesture: Boolean, resultMsg: Message): Boolean {
            if (view !== web || !isUserGesture) return false
            // A hit-test URL is not necessarily window.open's destination.
            // This unattached, network-blocked view captures the actual URL;
            // it never executes page script or becomes an authenticated window.
            val popup = WebView(this@MainActivity)
            popups.add(popup)
            popup.settings.apply {
                javaScriptEnabled = false
                domStorageEnabled = false
                allowFileAccess = false
                allowContentAccess = false
                blockNetworkLoads = true
                mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
            }
            CookieManager.getInstance().setAcceptThirdPartyCookies(popup, false)
            var consumed = false
            fun destination(url: String) {
                if (consumed || Uri.parse(url).scheme == "about") return
                consumed = true
                popup.stopLoading()
                root.post {
                    if (popups.remove(popup)) popup.destroy()
                    if (view === web && !isFinishing && !isDestroyed) {
                        if (OriginPolicy.isInternal(url, origin)) view.loadUrl(url)
                        else if (OriginPolicy.isHttps(url)) confirmExternal(url)
                    }
                }
            }
            popup.webViewClient = object : WebViewClient() {
                override fun shouldOverrideUrlLoading(target: WebView, request: WebResourceRequest): Boolean {
                    destination(request.url.toString())
                    return true
                }
                override fun onPageStarted(target: WebView, url: String, favicon: Bitmap?) { destination(url) }
                override fun onReceivedError(target: WebView, request: WebResourceRequest, error: WebResourceError) {
                    if (request.isForMainFrame) destination(request.url.toString())
                }
                override fun shouldInterceptRequest(target: WebView, request: WebResourceRequest): WebResourceResponse {
                    if (request.isForMainFrame) runOnUiThread { destination(request.url.toString()) }
                    return WebResourceResponse("text/plain", "utf-8", java.io.ByteArrayInputStream(ByteArray(0)))
                }
            }
            val transport = resultMsg.obj as? WebView.WebViewTransport ?: run {
                popups.remove(popup); popup.destroy(); return false
            }
            transport.webView = popup
            resultMsg.sendToTarget()
            root.postDelayed({ if (popups.remove(popup)) { popup.stopLoading(); popup.destroy() } }, 5000)
            return true
        }

        override fun onShowFileChooser(view: WebView, callback: ValueCallback<Array<Uri>>, params: FileChooserParams): Boolean {
            if (view !== web || !OriginPolicy.isInternal(view.url ?: "", origin) || chooserLaunched) {
                callback.onReceiveValue(null)
                return true
            }
            cancelFileCallback()
            fileCallback = callback
            val accepted = params.acceptTypes.flatMap { it.split(',') }.mapNotNull { raw ->
                val type = raw.trim().lowercase()
                if (type.startsWith('.')) MimeTypeMap.getSingleton().getMimeTypeFromExtension(type.drop(1))
                else type.takeIf { it.matches(Regex("[a-z0-9.+-]+/([a-z0-9.+-]+|\\*)")) }
            }.distinct().toTypedArray()
            val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                addCategory(Intent.CATEGORY_OPENABLE)
                type = if (accepted.size == 1) accepted[0] else "*/*"
                if (accepted.size > 1) putExtra(Intent.EXTRA_MIME_TYPES, accepted)
                putExtra(Intent.EXTRA_ALLOW_MULTIPLE, params.mode == FileChooserParams.MODE_OPEN_MULTIPLE)
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
            return try {
                chooserLaunched = true
                @Suppress("DEPRECATION")
                startActivityForResult(intent, FILE_REQUEST)
                true
            } catch (_: ActivityNotFoundException) {
                chooserLaunched = false
                cancelFileCallback()
                Toast.makeText(this@MainActivity, "未找到系统文件选择器。", Toast.LENGTH_LONG).show()
                true
            }
        }
    }

    @Deprecated("System document picker callback, no persistent storage permission requested")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != FILE_REQUEST) return
        chooserLaunched = false
        val callback = fileCallback ?: return
        fileCallback = null
        val uris = if (resultCode == RESULT_OK) {
            val clips = data?.clipData
            (if (clips != null) (0 until minOf(clips.itemCount, 64)).map { clips.getItemAt(it).uri }
            else listOfNotNull(data?.data)).filter { it.scheme == "content" }.distinct().toTypedArray()
        } else emptyArray()
        callback.onReceiveValue(uris.takeIf { it.isNotEmpty() })
    }

    private fun cancelFileCallback() { fileCallback?.onReceiveValue(null); fileCallback = null }

    private fun showError(message: String) {
        progress.visibility = View.INVISIBLE
        errorText.text = message
        errorPanel.visibility = View.VISIBLE
    }

    private fun reload() {
        errorPanel.visibility = View.GONE
        val view = web ?: return
        if (OriginPolicy.isInternal(view.url ?: "", origin)) view.reload() else view.loadUrl("$origin/")
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        // Update the Activity theme before WebView receives the configuration:
        // its prefers-color-scheme derives from android:isLightTheme.
        theme.applyStyle(R.style.AppTheme, true)
        super.onConfigurationChanged(newConfig)
        applySystemTheme()
    }

    @Suppress("DEPRECATION")
    private fun applySystemTheme() {
        val light = resources.getBoolean(R.bool.light_system_bars)
        window.statusBarColor = bg
        window.navigationBarColor = if (Build.VERSION.SDK_INT >= 27 || !light) bg else Color.rgb(51, 51, 51)
        var flags = window.decorView.systemUiVisibility and View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR.inv()
        if (light) flags = flags or View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR
        if (Build.VERSION.SDK_INT >= 27) {
            flags = flags and View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR.inv()
            if (light) flags = flags or View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR
        }
        window.decorView.systemUiVisibility = flags
        window.setBackgroundDrawableResource(R.color.shell_background)
        root.setBackgroundColor(bg)
        errorPanel.setBackgroundColor(bg)
        tintText(root)
        val accent = ColorStateList.valueOf(getColor(R.color.shell_accent))
        progress.progressTintList = accent
        web?.setBackgroundColor(getColor(R.color.page_background))
        dialogs.toList().forEach(::tintDialog)
    }

    private fun tintText(view: View) {
        // Do not enter WebView: page styling belongs to the site's theme setting.
        if (view is WebView) return
        if (view is TextView) {
            view.setTextColor(textColor)
            if (view is EditText) {
                view.setHintTextColor(textColor)
                view.backgroundTintList = ColorStateList.valueOf(getColor(R.color.shell_accent))
            }
            if (view is Button) {
                val ripple = android.util.TypedValue()
                theme.resolveAttribute(android.R.attr.selectableItemBackground, ripple, true)
                view.setBackgroundResource(ripple.resourceId)
            }
        }
        if (view is ViewGroup) for (i in 0 until view.childCount) tintText(view.getChildAt(i))
    }

    private fun tintDialog(dialog: AlertDialog) {
        dialog.window?.let { window ->
            window.decorView.background?.mutate()?.setTint(bg)
            tintText(window.decorView)
        }
    }

    private fun showDialog(dialog: AlertDialog) {
        dialogs.add(dialog)
        dialog.setOnDismissListener { dialogs.remove(dialog) }
        dialog.show()
        tintDialog(dialog)
    }

    private fun confirmExternal(url: String) {
        if (!OriginPolicy.isHttps(url)) return
        AlertDialog.Builder(this)
            .setTitle("在系统浏览器中打开？")
            .setMessage("目标站点：${Uri.parse(url).host}\n不会把应用登录凭据传给浏览器。")
            .setNegativeButton("取消", null)
            .setPositiveButton("打开") { _, _ ->
                try { startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)).addCategory(Intent.CATEGORY_BROWSABLE)) }
                catch (_: ActivityNotFoundException) { Toast.makeText(this, "未找到浏览器。", Toast.LENGTH_SHORT).show() }
            }.create().let(::showDialog)
    }

    private fun showSites() {
        AlertDialog.Builder(this)
            .setTitle("站点设置")
            .setItems(arrayOf("正式站", "开发站（测试界面）", "自定义 HTTPS 站点", "取消")) { _, index ->
                when (index) {
                    0 -> confirmSite(OriginPolicy.PRODUCTION)
                    1 -> confirmSite(OriginPolicy.DEVELOPMENT)
                    2 -> customSite()
                }
            }.create().let(::showDialog)
    }

    private fun customSite() {
        val input = EditText(this).apply {
            setSingleLine()
            setText(origin)
            contentDescription = "HTTPS 站点地址"
            inputType = android.text.InputType.TYPE_CLASS_TEXT or android.text.InputType.TYPE_TEXT_VARIATION_URI
            setPadding(dp(20), dp(12), dp(20), dp(12))
        }
        val dialog = AlertDialog.Builder(this).setTitle("自定义 HTTPS 站点")
            .setMessage("只填写你信任的站点根地址，不含路径、账号密码或参数。")
            .setView(input).setNegativeButton("取消", null).setPositiveButton("继续", null).create()
        dialog.setOnShowListener {
            dialog.getButton(AlertDialog.BUTTON_POSITIVE).setOnClickListener {
                val selected = OriginPolicy.normalize(input.text.toString())
                if (selected == null) input.error = "请输入有效的 HTTPS 根地址，例如 https://work.bigfisharc.cn"
                else { dialog.dismiss(); confirmSite(selected) }
            }
        }
        showDialog(dialog)
    }

    private fun confirmSite(selected: String) {
        if (selected == origin) return
        AlertDialog.Builder(this).setTitle("切换连接站点？")
            .setMessage("$selected\n\n将离开当前页面并丢弃未发送草稿。已运行的服务端任务不会因此取消。仅向你信任的站点输入密码。")
            .setNegativeButton("取消", null)
            .setPositiveButton("切换") { _, _ ->
                if (preferences.edit().putString("origin", selected).commit()) {
                    origin = selected
                    createWebView()
                } else Toast.makeText(this, "站点设置保存失败，未切换。", Toast.LENGTH_LONG).show()
            }.create().let(::showDialog)
    }

    private fun navigateBack() {
        if (errorPanel.visibility == View.VISIBLE) errorPanel.visibility = View.GONE
        if (web?.canGoBack() == true) web?.goBack() else finish()
    }

    @Deprecated("API 33+ uses OnBackInvokedCallback")
    override fun onBackPressed() { navigateBack() }

    override fun onSaveInstanceState(outState: Bundle) {
        outState.putString("origin", origin)
        outState.putBundle("web", Bundle().also { web?.saveState(it) })
        super.onSaveInstanceState(outState)
    }

    override fun onPause() {
        web?.onPause()
        CookieManager.getInstance().flush()
        super.onPause()
    }

    override fun onResume() { super.onResume(); web?.onResume() }

    private fun clearPopups() {
        popups.forEach { it.stopLoading(); it.destroy() }
        popups.clear()
    }

    override fun onDestroy() {
        if (Build.VERSION.SDK_INT >= 33) backCallback?.let { onBackInvokedDispatcher.unregisterOnBackInvokedCallback(it) }
        dialogs.toList().forEach { it.dismiss() }
        cancelFileCallback()
        clearPopups()
        web?.let { content.removeView(it); it.stopLoading(); it.webChromeClient = null; it.destroy() }
        web = null
        super.onDestroy()
    }

    companion object { private const val FILE_REQUEST = 701 }
}
