"""
Tests for crates/daemon/static/index.html
Verifies all spec requirements are met.
"""

import os
import re

HTML_PATH = os.path.join(os.path.dirname(__file__), "index.html")


def read_html():
    with open(HTML_PATH, encoding="utf-8") as f:
        return f.read()


def test_file_exists():
    assert os.path.isfile(HTML_PATH), f"index.html not found at {HTML_PATH}"


def test_title():
    html = read_html()
    assert "<title>rage</title>" in html


def test_no_external_cdn():
    html = read_html()
    # No CDN script references
    assert "cdn." not in html.lower(), "No CDN references allowed"
    assert "unpkg.com" not in html, "No unpkg CDN references allowed"
    assert "jsdelivr.net" not in html, "No jsDelivr CDN references allowed"
    assert "cdnjs.cloudflare.com" not in html, "No cdnjs CDN references allowed"


def test_no_framework_imports():
    html = read_html()
    frameworks = ["react", "vue", "angular", "svelte", "ember", "backbone"]
    for fw in frameworks:
        assert fw not in html.lower(), f"Framework '{fw}' import found — not allowed"


def test_css_variables():
    html = read_html()
    assert "--bg" in html
    assert "#0d0d10" in html
    assert "--fg" in html
    assert "#e8e8eb" in html
    assert "--muted" in html
    assert "#888" in html
    assert "--ok" in html
    assert "#4caf50" in html
    assert "--warn" in html
    assert "#ffb74d" in html
    assert "--err" in html
    assert "#ef5350" in html
    assert "--converging" in html
    assert "#64b5f6" in html


def test_body_styling():
    html = read_html()
    # Font stack
    assert "Apple" in html or "-apple-system" in html
    assert "Segoe UI" in html
    assert "monospace" in html
    # Dark background
    assert "var(--bg)" in html
    # 24px padding
    assert "24px" in html


def test_header_element():
    html = read_html()
    assert re.search(r"<header[^>]*>.*?rage.*?</header>", html, re.DOTALL), \
        "Header element with 'rage' text not found"


def test_ws_status_div():
    html = read_html()
    assert 'id="ws-status"' in html or "id='ws-status'" in html
    assert "connecting" in html


def test_state_div():
    html = read_html()
    assert 'id="state"' in html or "id='state'" in html
    assert "idle" in html


def test_tasks_div():
    html = read_html()
    assert 'id="tasks"' in html or "id='tasks'" in html


def test_state_classes():
    html = read_html()
    for cls in [".state.idle", ".state.converging", ".state.ready", ".state.blocked"]:
        assert cls in html, f"CSS class '{cls}' not found"


def test_task_classes():
    html = read_html()
    for cls in [".task.waiting", ".task.running", ".task.ok", ".task.failed"]:
        assert cls in html, f"CSS class '{cls}' not found"
    assert ".name" in html
    assert ".meta" in html


def test_button_styling():
    html = read_html()
    assert "#2c2c33" in html
    assert "#3a3a42" in html


def test_use_strict():
    html = read_html()
    assert '"use strict"' in html or "'use strict'" in html


def test_dom_references():
    html = read_html()
    assert "stateEl" in html
    assert "tasksEl" in html
    assert "wsStatus" in html
    assert "#state" in html
    assert "#tasks" in html
    assert "#ws-status" in html


def test_socket_variable():
    html = read_html()
    assert "socket" in html


def test_connect_function():
    html = read_html()
    assert "function connect" in html or "connect()" in html
    # ws/wss protocol selection
    assert "wss:" in html
    assert "ws:" in html
    assert "location.protocol" in html
    assert "WebSocket" in html
    assert "onopen" in html
    assert "onclose" in html
    assert "onerror" in html
    assert "onmessage" in html
    assert "JSON.parse" in html
    assert "setTimeout" in html


def test_render_function():
    html = read_html()
    assert "function render" in html or "render(" in html
    assert "stateEl.className" in html
    assert "snap.state" in html
    assert "tasksEl.innerHTML" in html
    assert "snap.tasks" in html
    assert "renderTask" in html


def test_render_task_function():
    html = read_html()
    assert "function renderTask" in html or "renderTask(" in html
    assert "t.status" in html
    assert "t.package" in html
    assert "t.script" in html
    assert "duration_ms" in html
    assert "toFixed" in html
    assert "exit_code" in html
    assert "retry" in html


def test_retry_function():
    html = read_html()
    assert "function retry" in html or "retry(" in html
    assert "RetryTask" in html
    assert "JSON.stringify" in html or "send(" in html


def test_connect_called_at_end():
    html = read_html()
    # connect() should be called somewhere (at the end of the script)
    assert "connect()" in html


if __name__ == "__main__":
    import sys

    tests = [
        test_file_exists,
        test_title,
        test_no_external_cdn,
        test_no_framework_imports,
        test_css_variables,
        test_body_styling,
        test_header_element,
        test_ws_status_div,
        test_state_div,
        test_tasks_div,
        test_state_classes,
        test_task_classes,
        test_button_styling,
        test_use_strict,
        test_dom_references,
        test_socket_variable,
        test_connect_function,
        test_render_function,
        test_render_task_function,
        test_retry_function,
        test_connect_called_at_end,
    ]

    passed = 0
    failed = 0
    for t in tests:
        try:
            t()
            print(f"  PASS  {t.__name__}")
            passed += 1
        except Exception as e:
            print(f"  FAIL  {t.__name__}: {e}")
            failed += 1

    print(f"\n{passed}/{passed+failed} tests passed")
    if failed:
        sys.exit(1)
