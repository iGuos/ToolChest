use std::process::{Child, Command};
use std::sync::Mutex;

/// Holds the long-running watcher osascript child process.
pub struct WatcherProc(pub Mutex<Option<Child>>);

const WATCH_LOG: &str = "/tmp/baibao_watch.log";

// Long-lived "warm-loop" watcher: ONE osascript process keeps Chromium's
// accessibility tree warm and rescans ~once per second, clicking any approval
// dialog and appending the result to WATCH_LOG. This is far more reliable than
// spawning a fresh (cold-tree) osascript per tick — a freshly enabled tree
// often reports 0 buttons, which is what caused earlier misses. The marker
// comment lets us pkill orphaned loops left by dev hot-reloads.
const WATCH_SCRIPT: &str = r#"
-- baibaoWatchLoopMarker
set targetBtns to {"Allow", "Always Allow", "Allow Always", "Allow Once", "OK", "允许", "始终允许", "仅允许一次", "确认", "Approve", "Yes", "Grant", "Continue", "Accept", "Authorize", "继续", "授权"}
set logFile to "/tmp/baibao_watch.log"
repeat 1000000 times
    try
        tell application "System Events"
            repeat with appName in {"Claude", "Claude Code"}
                if exists (processes where name is appName) then
                    tell process appName
                        try
                            set value of attribute "AXManualAccessibility" to true
                        end try
                        try
                          with timeout of 20 seconds
                            repeat with w in every window
                                set theElems to entire contents of w
                                set clicked to false
                                repeat with i from (count of theElems) to 1 by -1
                                    set e to item i of theElems
                                    try
                                        if (role of e) is "AXButton" then
                                            set bn to name of e
                                            if bn is not missing value and bn does not contain "?" then
                                                repeat with t in targetBtns
                                                    if bn starts with t then
                                                        try
                                                            perform action "AXPress" of e
                                                        on error
                                                            click e
                                                        end try
                                                        do shell script "printf '%s\\n' " & quoted form of ("Clicked '" & bn & "' [" & (appName as text) & "]") & " >> " & quoted form of logFile
                                                        set clicked to true
                                                        exit repeat
                                                    end if
                                                end repeat
                                            end if
                                        end if
                                    end try
                                    if clicked then exit repeat
                                end repeat
                                if clicked then exit repeat
                            end repeat
                          end timeout
                        end try
                    end tell
                end if
            end repeat
        end tell
    end try
    delay __DELAY__
end repeat
"#;

// Electron/Chromium does NOT expose its web content to the macOS Accessibility
// API by default. The canonical switch for Chromium is AXManualAccessibility
// (AXEnhancedUserInterface is the old VoiceOver-only flag newer Chromium ignores).
// We set both, wrap traversal in a timeout (entire contents on a large web tree
// can hang), and match button names by PREFIX so a label carrying a keyboard-
// shortcut glyph like "Allow once ⌘⏎" still matches "Allow Once".
const CLICK_SCRIPT: &str = r#"
set targetBtns to {"Allow", "Always Allow", "Allow Always", "Allow Once",
    "OK", "允许", "始终允许", "仅允许一次", "确认", "Approve",
    "Yes", "Grant", "Continue", "Accept", "Authorize", "继续", "授权"}
set results to {}
try
    tell application "System Events"
        set targetApps to {"Claude", "Claude Code"}
        repeat with appName in targetApps
            if exists (processes where name is appName) then
                tell process appName
                    -- Force-enable Chromium accessibility tree, then let it build
                    try
                        set value of attribute "AXManualAccessibility" to true
                    end try
                    try
                        set value of attribute "AXEnhancedUserInterface" to true
                    end try
                    delay 0.4
                    set clicked to false
                    -- A freshly-enabled Chromium tree is often "cold": elements present but
                    -- 0 buttons exposed (this is what caused earlier misses). Keep THIS process
                    -- alive and rescan until buttons appear — the tree warms up across iterations.
                    repeat 4 times
                        set sawButton to false
                        with timeout of 60 seconds
                            repeat with w in every window
                                try
                                    -- Iterate from the END: permission buttons sit at the bottom
                                    -- of the chat (tail of the tree), so reverse order finds them
                                    -- in a few steps and reaches "Allow once" before "Always allow",
                                    -- preferring single-use approval.
                                    set theElems to entire contents of w
                                    repeat with i from (count of theElems) to 1 by -1
                                        set e to item i of theElems
                                        try
                                            if (role of e) is "AXButton" then
                                                set sawButton to true
                                                set btnName to name of e
                                                if btnName is not missing value and btnName does not contain "?" then
                                                    repeat with t in targetBtns
                                                        if btnName starts with t then
                                                            try
                                                                perform action "AXPress" of e
                                                                set end of results to "Clicked '" & btnName & "' [" & appName & "]"
                                                                set clicked to true
                                                            on error pressErr
                                                                try
                                                                    click e
                                                                    set end of results to "Clicked(click) '" & btnName & "' [" & appName & "]"
                                                                    set clicked to true
                                                                on error
                                                                    set end of results to "MATCH '" & btnName & "' but press FAILED: " & pressErr
                                                                end try
                                                            end try
                                                            exit repeat
                                                        end if
                                                    end repeat
                                                end if
                                            end if
                                        end try
                                        if clicked then exit repeat
                                    end repeat
                                    if clicked then exit repeat
                                on error eMsg
                                    set end of results to "scan ERROR [" & appName & "]: " & eMsg
                                end try
                            end repeat
                        end timeout
                        if clicked then exit repeat
                        -- Warm tree with buttons but no dialog: done, don't waste rescans.
                        if sawButton then exit repeat
                        -- Cold tree (0 buttons): wait briefly and rescan.
                        delay 0.6
                    end repeat
                end tell
            end if
        end repeat
    end tell
end try
return results
"#;

const DUMP_SCRIPT: &str = r#"
set results to {}
try
    tell application "System Events"
        set uiEnabled to UI elements enabled
        set end of results to "UI elements enabled: " & uiEnabled
        set targetApps to {"Claude", "Claude Code"}
        repeat with appName in targetApps
            if exists (processes where name is appName) then
                tell process appName
                    -- Force-enable Chromium accessibility tree, then let it build
                    try
                        set value of attribute "AXManualAccessibility" to true
                    end try
                    try
                        set value of attribute "AXEnhancedUserInterface" to true
                    end try
                    delay 0.4
                    set winCount to count of every window
                    set end of results to "[" & appName & "] windows: " & winCount
                    with timeout of 30 seconds
                        repeat with w in every window
                            try
                                set winName to name of w
                            on error
                                set winName to "(no name)"
                            end try
                            try
                                set t0 to (current date)
                                set theElems to entire contents of w
                                set elemCount to (count of theElems)
                                set cBtn to 0
                                set cText to 0
                                set cGroup to 0
                                set cOther to 0
                                set cErr to 0
                                repeat with i from 1 to elemCount
                                    set e to item i of theElems
                                    set r to "ERR"
                                    try
                                        set r to (role of e) as string
                                    on error
                                        set cErr to cErr + 1
                                    end try
                                    if r is "AXButton" then
                                        set cBtn to cBtn + 1
                                    else if r is "AXStaticText" then
                                        set cText to cText + 1
                                    else if r is "AXGroup" then
                                        set cGroup to cGroup + 1
                                    else if r is not "ERR" then
                                        set cOther to cOther + 1
                                    end if
                                    -- dump the tail, where the permission dialog should live
                                    if i > (elemCount - 15) then
                                        set nm to "?"
                                        try
                                            set nm to (name of e) as string
                                        end try
                                        set end of results to "    [" & i & "] " & r & " '" & nm & "'"
                                    end if
                                end repeat
                                set elapsed to (current date) - t0
                                set end of results to "  win='" & winName & "' elems=" & elemCount & " btn=" & cBtn & " text=" & cText & " group=" & cGroup & " other=" & cOther & " err=" & cErr & " " & elapsed & "s"
                            on error errMsg number errNum
                                set end of results to "  win='" & winName & "' ENUM-ERROR " & errNum & ": " & errMsg
                            end try
                        end repeat
                    end timeout
                end tell
            else
                set end of results to "[" & appName & "] not running"
            end if
        end repeat
    end tell
end try
if length of results is 0 then
    set results to {"(System Events returned nothing — Accessibility permission likely missing)"}
end if
return results
"#;

fn parse_as_list(raw: &str) -> Vec<String> {
    let s = raw.trim();
    if s.is_empty() || s == "{}" {
        return vec![];
    }
    let inner = s.trim_start_matches('{').trim_end_matches('}').trim();
    if inner.is_empty() {
        return vec![];
    }
    inner
        .split("\", \"")
        .map(|s| s.trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn run_script(script: &str) -> Result<String, String> {
    let out = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| format!("osascript failed: {e}"))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[tauri::command]
pub async fn check_accessibility() -> bool {
    // osascript 启动有 ~100-300ms 开销，放到阻塞线程池避免冻结主线程。
    tauri::async_runtime::spawn_blocking(|| {
        let script =
            r#"tell application "System Events" to return (UI elements enabled) as string"#;
        matches!(run_script(script), Ok(s) if s.trim() == "true")
    })
    .await
    .unwrap_or(false)
}

#[tauri::command]
pub fn get_binary_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

#[tauri::command]
pub async fn check_claude_dialogs() -> Result<Vec<String>, String> {
    let raw = run_script(CLICK_SCRIPT)?;
    Ok(parse_as_list(&raw))
}

#[tauri::command]
pub async fn dump_claude_buttons() -> Result<Vec<String>, String> {
    let raw = run_script(DUMP_SCRIPT)?;
    Ok(parse_as_list(&raw))
}

#[tauri::command]
pub fn open_accessibility_settings() -> Result<(), String> {
    Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Start the persistent warm-loop watcher (idempotent).
/// `interval` is the scan cadence in seconds (smaller = faster detection);
/// it is injected into the loop's `delay` and clamped to a sane range.
#[tauri::command]
pub fn start_watcher(interval: f64, state: tauri::State<WatcherProc>) -> Result<(), String> {
    let mut guard = state.0.lock().map_err(|e| e.to_string())?;
    if guard.is_some() {
        return Ok(());
    }
    // Reap orphaned loops from a previous run (e.g. dev hot-reload), then reset log.
    let _ = Command::new("pkill")
        .args(["-f", "baibaoWatchLoopMarker"])
        .status();
    let _ = std::fs::write(WATCH_LOG, b"");
    // Clamp: below ~0.2s the warm loop just pegs CPU; above 60s is pointless.
    let delay = if interval.is_finite() {
        interval.clamp(0.2, 60.0)
    } else {
        1.0
    };
    let script = WATCH_SCRIPT.replace("__DELAY__", &format!("{delay}"));
    let child = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .spawn()
        .map_err(|e| format!("启动监听进程失败: {e}"))?;
    *guard = Some(child);
    Ok(())
}

/// Stop the persistent watcher.
#[tauri::command]
pub fn stop_watcher(state: tauri::State<WatcherProc>) -> Result<(), String> {
    let mut guard = state.0.lock().map_err(|e| e.to_string())?;
    if let Some(mut child) = guard.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = Command::new("pkill")
        .args(["-f", "baibaoWatchLoopMarker"])
        .status();
    Ok(())
}

/// Drain new "Clicked …" lines the watcher loop has written since last poll.
#[tauri::command]
pub fn read_watcher_clicks() -> Vec<String> {
    // 原子地把日志改名取走再读，避免「读出 → 清空」之间后端 append 的记录被丢掉：
    // 改名后，监听脚本下一次 `>>` append 会重建原文件，下个 tick 再取，零丢失。
    let drained = format!("{WATCH_LOG}.drain");
    if std::fs::rename(WATCH_LOG, &drained).is_err() {
        return vec![]; // 日志文件还不存在
    }
    let content = std::fs::read_to_string(&drained).unwrap_or_default();
    let _ = std::fs::remove_file(&drained);
    content
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}
