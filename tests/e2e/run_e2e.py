#!/usr/bin/env python3
"""ssh-mcp E2E 測試：以原始 JSON-RPC（stdio）驅動 server，跑 PLAN §10 驗收案例。

前置：先執行 tests/e2e/setup_linux.sh <work_dir>（建立 sshd、ssh-agent、config.toml）。

用法：
    source <work_dir>/agent.env
    python3 tests/e2e/run_e2e.py <ssh-mcp 執行檔> <work_dir>
"""

import json
import os
import subprocess
import sys
import time

EXE, WORK = sys.argv[1], sys.argv[2]
CONFIG = os.path.join(WORK, "config.toml")

results = []  # (name, ok, detail)


class Mcp:
    """最小 MCP client：newline-delimited JSON-RPC over stdio。"""

    def __init__(self, extra_env=None):
        env = dict(os.environ)
        env.update(extra_env or {})
        self.p = subprocess.Popen(
            [EXE, "--config", CONFIG],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=open(os.path.join(WORK, "logs", "server-stderr.log"), "a"),
            env=env,
            text=True,
            bufsize=1,
        )
        self.next_id = 1
        self.non_json_lines = []
        self.call("initialize", {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "e2e", "version": "0"},
        })
        self.notify("notifications/initialized")

    def notify(self, method, params=None):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def call(self, method, params):
        rid = self.next_id
        self.next_id += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("server 關閉了 stdout")
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                self.non_json_lines.append(line)  # stdout 被污染
                continue
            if msg.get("id") == rid:
                if "error" in msg:
                    raise RuntimeError(f"JSON-RPC error: {msg['error']}")
                return msg["result"]

    def tool(self, name, args=None):
        r = self.call("tools/call", {"name": name, "arguments": args or {}})
        sc = r.get("structuredContent")
        text = "".join(c.get("text", "") for c in r.get("content", []) if c.get("type") == "text")
        return r.get("isError", False), sc, text

    def exec(self, host, command, **kw):
        return self.tool("ssh_exec", {"host": host, "command": command, **kw})

    def close(self):
        self.p.stdin.close()
        try:
            self.p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.p.kill()


def check(name, cond, detail=""):
    results.append((name, bool(cond), detail))
    print(("PASS " if cond else "FAIL ") + name + ("" if cond else f"\n     {detail}"))


def main():
    m = Mcp()

    # ---- 協定：工具清單與 annotations ----
    tools = {t["name"]: t for t in m.call("tools/list", {})["tools"]}
    check("tools/list 有 ssh_list_hosts 與 ssh_exec", set(tools) == {"ssh_list_hosts", "ssh_exec"}, list(tools))
    ann = tools["ssh_exec"].get("annotations", {})
    check("ssh_exec 標記 destructiveHint", ann.get("destructiveHint") is True, ann)
    check("ssh_list_hosts 標記 readOnlyHint", tools["ssh_list_hosts"].get("annotations", {}).get("readOnlyHint") is True)

    # ---- ssh_list_hosts ----
    err, sc, _ = m.tool("ssh_list_hosts")
    hosts = {h["alias"]: h for h in sc["hosts"]}
    check("list_hosts 顯示 port 2222", hosts["pi-2222"]["port"] == 2222, hosts["pi-2222"])
    check("list_hosts 未設定 port → defaults 2201", hosts["build"]["port"] == 2201)
    check("list_hosts 顯示初始 cwd", hosts["build"]["cwd"] == "/home/builder", hosts["build"])

    # ---- 1. stdout / stderr / exit code ----
    err, sc, _ = m.exec("pi-2222", "echo hi; echo err >&2; exit 3")
    check("案例1 stdout/stderr/exit_code", (not err) and sc["stdout"] == "hi\n" and sc["stderr"] == "err\n" and sc["exit_code"] == 3, sc)
    check("案例1 exit 時 cwd_updated=false", sc["cwd_updated"] is False, sc)

    # ---- 2. timeout ----
    t0 = time.time()
    err, sc, _ = m.exec("pi-2222", "echo start; sleep 100", timeout_s=2)
    dt = time.time() - t0
    check("案例2 timeout 約 2 秒回傳", sc["timed_out"] is True and 1.5 < dt < 6 and "start" in sc["stdout"], (dt, sc))

    # ---- 3. 大量輸出截斷 ----
    err, sc, _ = m.exec("pi-2222", "seq 1 100000")
    path = sc.get("full_output_path")
    full_ok = path and os.path.exists(path) and "\n100000\n" in open(path).read()
    check("案例3 截斷 + 完整輸出存檔", sc["truncated"] and full_ok and len(sc["stdout"]) < 5000, {k: sc[k] for k in ("truncated", "full_output_path")})
    check("案例3/6h 截斷時 cwd 仍更新", sc["cwd_updated"] is True, sc["cwd"])

    # ---- 4. 未知主機 ----
    err, sc, text = m.exec("nope", "true")
    check("案例4 未知 alias → isError 並列出可用 alias", err and "pi-2222" in text, text)

    # ---- 5. 主機金鑰不在 known_hosts ----
    err, sc, text = m.exec("unknown-key", "true")
    check("案例5 known_hosts 沒有此主機 → 拒絕", err and "known_hosts" in text, text)

    # ---- 6. guard ----
    err, sc, text = m.exec("pi-2222", "rm -rf /")
    check("案例6 rm -rf / 被擋", err and "安全規則" in text, text)
    err, sc, text = m.exec("danger", "echo reboot")
    check("案例6 allow_dangerous 但未設環境變數 → 仍擋", err, text)

    # ---- 6a. 非 22 port 與 defaults ----
    err, sc, _ = m.exec("pi-2222", "whoami")
    check("案例6a port 2222 連線", sc["stdout"].strip() == "pi", sc)

    # ---- 6b. 同主機不同帳號 ----
    err, sc, _ = m.exec("builder-2222", "whoami")
    check("案例6b 同主機不同 alias 用不同帳號", sc["stdout"].strip() == "builder", sc)

    # ---- 6c. cwd 記憶 ----
    m.exec("pi-2222", "cd /tmp")
    err, sc, _ = m.exec("pi-2222", "pwd")
    check("案例6c cd 後記住目錄", sc["stdout"].strip() == "/tmp" and sc["cwd"] == "/tmp", sc)
    m.exec("pi-2222", "cd ..")
    err, sc, _ = m.exec("pi-2222", "pwd")
    check("案例6c cd .. 正確", sc["stdout"].strip() == "/", sc)
    err, sc, _ = m.exec("pi-2222", "pwd", cwd="/usr")
    check("案例6c cwd 參數", sc["stdout"].strip() == "/usr" and sc["cwd"] == "/usr", sc)
    err, sc, _ = m.exec("pi-2222", "pwd", cwd="share")
    check("案例6c 相對 cwd 參數", sc["stdout"].strip() == "/usr/share", sc)
    err, sc, _ = m.exec("pi-2222", "pwd", reset_cwd=True)
    check("案例6c reset_cwd 回到登入目錄", sc["stdout"].strip() == "/home/pi", sc)
    m.exec("build", "cd /var")
    err, sc, _ = m.exec("build", "pwd", reset_cwd=True)
    check("案例6c reset_cwd 回到設定檔 cwd", sc["stdout"].strip() == "/home/builder", sc)

    # ---- 6d. exit 時不更新 cwd ----
    m.exec("pi-2222", "cd /tmp")
    err, sc, _ = m.exec("pi-2222", "cd /etc; exit 0")
    check("案例6d exit 時 cwd_updated=false 且維持原目錄", sc["cwd_updated"] is False and sc["cwd"] == "/tmp", sc)

    # ---- 6e. 記住的目錄被刪 ----
    m.exec("pi-2222", "mkdir -p /tmp/e2e-gone && cd /tmp/e2e-gone")
    m.exec("pi-2222", "rmdir /tmp/e2e-gone")
    err, sc, text = m.exec("pi-2222", "pwd")
    check("案例6e 目錄被刪 → isError 並提示 reset_cwd", err and "reset_cwd" in json.dumps(sc, ensure_ascii=False), sc)
    m.exec("pi-2222", "true", reset_cwd=True)

    # ---- 6f. env / pre_command ----
    err, sc, _ = m.exec("build", 'echo "$BB_NUMBER_THREADS|$LANG|$FROM_PRE|$QUOTED"')
    check("案例6f env + pre_command 生效（含單引號跳脫）", sc["stdout"].strip() == "8|C.UTF-8|yes|it's $HOME", sc)
    err, sc, _ = m.exec("badpre", "echo SHOULD_NOT_RUN")
    check("案例6f pre_command 失敗 → isError 且命令未執行", err and "SHOULD_NOT_RUN" not in sc["stdout"] and "pre_command" in sc.get("error", ""), sc)

    # ---- 6g. pre_command 自己 cd ----
    err, sc, _ = m.exec("build", "pwd")
    check("案例6g pre_command 會 cd，命令仍在記住的目錄", sc["stdout"].strip() == "/home/builder", sc)

    # ---- 6h. 標記不外洩 ----
    leaked = []
    for cmd in ["echo a", "printf noeol", "seq 1 50000", "cd /tmp; exit 0"]:
        _, sc, _ = m.exec("build", cmd)
        if "__SSHMCP_" in sc["stdout"] + sc["stderr"]:
            leaked.append(cmd)
    check("案例6h stdout/stderr 不含 __SSHMCP_ 標記", not leaked, leaked)
    _, sc, _ = m.exec("build", "printf noeol")
    check("案例6h 沒有結尾換行的輸出保持原樣", sc["stdout"] == "noeol", repr(sc["stdout"]))

    # ---- 6i. raw 模式 ----
    err, sc, _ = m.exec("raw", "pwd")
    check("案例6i raw 模式原樣執行、cwd 為 null", sc["stdout"].strip() == "/home/builder" and sc["cwd"] is None, sc)

    # ---- dash 登入 shell ----
    err, sc, _ = m.exec("dash", "cd /tmp; echo $FROM_PRE")
    err2, sc2, _ = m.exec("dash", "pwd")
    check("dash 登入 shell：包裝 script 可用", sc["stdout"].strip() == "dash" and sc2["stdout"].strip() == "/tmp", (sc, sc2))

    # ---- 其他：多行命令、# 註解、使用者自己回傳 254 ----
    err, sc, _ = m.exec("pi-2222", "echo one\necho two # comment")
    check("多行命令與結尾註解", sc["stdout"] == "one\ntwo\n" and sc["cwd_updated"], sc)
    err, sc, _ = m.exec("pi-2222", "(exit 254)")
    check("使用者命令回傳 254 不被誤判為 server 錯誤", (not err) and sc["exit_code"] == 254, sc)
    err, sc, _ = m.exec("pi-2222", "cat")  # 讀 stdin：應立即收到 EOF
    check("讀 stdin 的命令不會卡住", sc["timed_out"] is False and sc["exit_code"] == 0, sc)
    err, sc, _ = m.exec("pi-2222", "kill -9 $$")
    check("被 signal 終止時回報 exit_signal", sc["exit_signal"] is not None or sc["exit_code"] in (137, None), sc)

    # ---- 連線重用：重啟 sshd 後自動重連 ----
    pid = open(os.path.join(WORK, "sshd.pid")).read().strip()
    subprocess.run(["kill", pid])
    time.sleep(0.5)
    subprocess.run(["/usr/sbin/sshd", "-f", os.path.join(WORK, "sshd_config"), "-E", os.path.join(WORK, "logs", "sshd.log")], check=True)
    time.sleep(0.5)
    err, sc, text = m.exec("pi-2222", "echo back")
    check("sshd 重啟後自動重連", (not err) and sc["stdout"] == "back\n", (err, sc, text))

    # ---- stdout 純淨 ----
    check("案例7 stdout 只有 JSON-RPC", not m.non_json_lines, m.non_json_lines[:3])
    m.close()

    # ---- 危險命令放行：主機 allow_dangerous + 環境變數 ----
    m2 = Mcp({"SSH_MCP_ALLOW_DANGEROUS": "1"})
    err, sc, text = m2.exec("danger", "echo reboot")
    check("allow_dangerous + 環境變數 → 放行", (not err) and sc["stdout"] == "reboot\n", (err, sc, text))
    err, sc, text = m2.exec("pi-2222", "echo reboot")
    check("其他主機仍擋", err, text)
    m2.close()

    # ---- 稽核記錄 ----
    audit = [f for f in os.listdir(os.path.join(WORK, "logs")) if f.startswith("audit-")]
    lines = []
    for f in audit:
        lines += [json.loads(l) for l in open(os.path.join(WORK, "logs", f))]
    check("稽核記錄有 blocked 與 dangerous_allowed", any(l["blocked"] for l in lines) and any(l["dangerous_allowed"] for l in lines), len(lines))

    failed = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(failed)}/{len(results)} passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
