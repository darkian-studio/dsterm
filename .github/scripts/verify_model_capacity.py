#!/usr/bin/env python3
"""Replicate the M17 capacity-verification method against a bigger model by
driving the real dsterm server (same llama.cpp shim sampler + chat template
path as on-device).

Method (mirrors the on-device session, dsterm/.tmp/ticket.md §2/§3):
  A   : bare "hey", no system prompt
  S   : tiny system prompt
  H   : first 2798 chars of the original system prompt (bisection)
  N   : full original (negation-wording) system prompt  -> collapse baseline
  P   : full positive-form rewrite system prompt        -> style-controlled pair
  N2  : repeat of N                                     -> determinism check

Sampler handling (recorded in the report): payload temperature is 0.55 (the
app's slider value); the shim chain is penalties -> temp -> top_k -> top_p ->
min_p -> greedy with no dist() (dsterm_shim.c dsterm_llama_sampler_new), so
single-shot runs are conclusive and N vs N2 must be byte-identical.

usage: verify_model_capacity.py --model <gguf> --prompts-dir <dir>
       [--host 127.0.0.1] [--port 8767] [--max-tokens 256] [--out results.md]
"""

import argparse
import base64
import json
import os
import platform
import socket
import struct
import sys
import time
import urllib.request

DEGENERATE = {
    "none",
    "none.",
    "no",
    "no.",
    "-",
    "- no changes needed.",
    "- no changes needed",
    "null",
    "n/a",
    "n/a.",
    "not applicable",
    "nothing",
    "no tools needed.",
    "[]",
    "{}",
}


def http_post(host, port, path, body):
    req = urllib.request.Request(
        f"http://{host}:{port}{path}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=1800) as resp:
        return json.loads(resp.read())


SOCK_TIMEOUT = 900


def ws_connect(host, port, path):
    s = socket.create_connection((host, port), timeout=30)
    s.settimeout(SOCK_TIMEOUT)
    key = base64.b64encode(os.urandom(16)).decode()
    s.sendall(
        (
            f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        ).encode()
    )
    resp = b""
    while b"\r\n\r\n" not in resp:
        resp += s.recv(4096)
    head, _, rest = resp.partition(b"\r\n\r\n")
    if b"101" not in head.split(b"\r\n", 1)[0]:
        raise SystemExit("websocket handshake failed")
    return s, rest


def ws_send_text(s, text):
    data = text.encode()
    mask = os.urandom(4)
    header = bytearray([0x81])
    n = len(data)
    if n < 126:
        header.append(0x80 | n)
    elif n < 65536:
        header.append(0x80 | 126)
        header += struct.pack(">H", n)
    else:
        header.append(0x80 | 127)
        header += struct.pack(">Q", n)
    s.sendall(bytes(header) + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))


def mem_stats():
    with open("/proc/meminfo") as f:
        data = {}
        for line in f:
            k, _, v = line.partition(":")
            data[k] = int(v.split()[0])
        return {
            "total_gib": data["MemTotal"] / 1048576.0,
            "used_gib": (data["MemTotal"] - data["MemAvailable"]) / 1048576.0,
        }


def run(host, port, label, messages, max_tokens):
    session = http_post(host, port, "/ai/sessions", {})
    session_id = session.get("data", {}).get("session_id", "")
    payload = {
        "session_id": session_id,
        "messages": messages,
        "temperature": 0.55,
        "max_tokens": max_tokens,
        "stream": True,
    }

    sock, buf = ws_connect(host, port, "/ai/generate-stream")
    t0 = time.monotonic()
    ws_send_text(sock, json.dumps(payload))
    first_token = None
    usage = {}
    text_out = []
    tool_calls = []
    mem_first = None
    closed = False
    error = None
    try:
        while not closed:
            b = sock.recv(65536)
            if not b:
                break
            buf += b
            while len(buf) >= 2:
                opcode = buf[0] & 0x0F
                ln = buf[1] & 0x7F
                off = 2
                if ln == 126:
                    if len(buf) < 4:
                        break
                    ln = struct.unpack(">H", buf[2:4])[0]
                    off = 4
                elif ln == 127:
                    if len(buf) < 10:
                        break
                    ln = struct.unpack(">Q", buf[2:10])[0]
                    off = 10
                if len(buf) < off + ln:
                    break
                frame = buf[off:off + ln]
                buf = buf[off + ln:]
                if opcode == 8:
                    closed = True
                    break
                if opcode != 1:
                    continue
                try:
                    evt = json.loads(frame.decode(errors="replace"))
                except ValueError:
                    continue
                t = evt.get("type")
                if t == "text":
                    if first_token is None:
                        first_token = time.monotonic() - t0
                        mem_first = mem_stats()
                    text_out.append(evt.get("text", ""))
                elif t == "tool_call":
                    data = evt.get("data", {})
                    tool_calls.append(
                        (data.get("function_name"), data.get("arguments"))
                    )
                elif t == "usage":
                    usage = {
                        "prompt_tokens": evt.get("prompt_tokens", 0),
                        "completion_tokens": evt.get("completion_tokens", 0),
                    }
                elif t == "error":
                    error = evt.get("message") or frame.decode(errors="replace")
    finally:
        sock.close()
        try:
            http_post(host, port, "/ai/sessions/release", {"session_id": session_id})
        except Exception:
            pass

    wall = time.monotonic() - t0
    return {
        "label": label,
        "messages": messages,
        "reply": "".join(text_out),
        "tool_calls": tool_calls,
        "wall": wall,
        "ttft": first_token,
        "prompt_tokens": usage.get("prompt_tokens"),
        "completion_tokens": usage.get("completion_tokens"),
        "mem_first": mem_first,
        "error": error,
    }


def classify(reply, completion_tokens):
    t = reply.strip().lower()
    if not t:
        return "EMPTY"
    if t in DEGENERATE or t.startswith("none") or t.startswith("no changes"):
        return "COLLAPSE-LIKE"
    toks = completion_tokens or len(t.split())
    if toks <= 4:
        return "TERSE(%d)" % toks
    return "ok"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--model", required=True, help="path to the GGUF to load via /ai/load")
    ap.add_argument("--prompts-dir", required=True, help="dir with system_prompt_original.txt / system_prompt_positive.txt")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8767)
    ap.add_argument("--max-tokens", type=int, default=256)
    ap.add_argument("--out", default="results.md")
    args = ap.parse_args()

    tiny = "You are a helpful assistant."
    with open(os.path.join(args.prompts_dir, "system_prompt_original.txt")) as f:
        sp_original = f.read()
    with open(os.path.join(args.prompts_dir, "system_prompt_positive.txt")) as f:
        sp_positive = f.read()
    half = sp_original[:2798]

    tests = [
        ("A", [{"role": "user", "content": "hey"}]),
        ("S", [{"role": "system", "content": tiny}, {"role": "user", "content": "hey"}]),
        ("H", [{"role": "system", "content": half}, {"role": "user", "content": "hey"}]),
        ("N", [{"role": "system", "content": sp_original}, {"role": "user", "content": "hey"}]),
        ("P", [{"role": "system", "content": sp_positive}, {"role": "user", "content": "hey"}]),
        ("N2", [{"role": "system", "content": sp_original}, {"role": "user", "content": "hey"}]),
    ]

    mem0 = mem_stats()
    print(f"loading {args.model} ...")
    load_t0 = time.monotonic()
    load_resp = http_post(args.host, args.port, "/ai/load", {"path": args.model})
    print(f"load took {time.monotonic() - load_t0:.1f}s")

    results = []
    for key, msgs in tests:
        print(f"\n=== {key} ===")
        try:
            r = run(args.host, args.port, key, msgs, args.max_tokens)
        except Exception as exc:
            r = {
                "label": key,
                "messages": msgs,
                "reply": "",
                "tool_calls": [],
                "wall": None,
                "ttft": None,
                "prompt_tokens": None,
                "completion_tokens": None,
                "mem_first": None,
                "error": f"{type(exc).__name__}: {exc}",
            }
        if r["error"]:
            print("ERROR:", r["error"])
        else:
            print(f"reply: {r['reply']!r}")
            print(
                f"prompt={r['prompt_tokens']} completion={r['completion_tokens']} "
                f"ttft={r['ttft']:.1f}s wall={r['wall']:.1f}s"
            )
        results.append(r)

    determinism = "PASS" if results[3]["reply"] == results[5]["reply"] else "FAIL"

    lines = [
        "# Model capacity verification",
        "",
        f"- **model**: `{args.model}`",
        f"- **date (UTC)**: {time.strftime('%Y-%m-%d %H:%M:%S', time.gmtime())}",
        f"- **runner**: {platform.platform()} / {platform.machine()}, "
        f"{os.cpu_count()} cpus, {mem0['total_gib']:.1f} GiB RAM",
        f"- **sampler**: temperature 0.55 (app slider); shim chain penalties -> temp "
        f"-> top_k -> top_p -> min_p -> **greedy**, no dist() -> deterministic",
        f"- **max completion tokens per test**: {args.max_tokens}",
        "",
        "| test | prompt tokens | completion tokens | TTFT (s) | wall (s) | verdict | reply |",
        "|---|---|---|---|---|---|---|",
    ]
    for r in results:
        verdict = classify(r["reply"], r["completion_tokens"]) if not r["error"] else "ERROR"
        reply = r["reply"].replace("|", "\\|").replace("\n", " ⏎ ")
        if len(reply) > 140:
            reply = reply[:140] + "…"
        ttft = f"{r['ttft']:.1f}" if r["ttft"] is not None else "-"
        wall = f"{r['wall']:.1f}" if r["wall"] is not None else "-"
        lines.append(
            f"| {r['label']} | {r['prompt_tokens']} | {r['completion_tokens']} | "
            f"{ttft} | {wall} | {verdict} | {reply} |"
        )
    lines += [
        "",
        f"**Determinism (N vs N2 byte-identical): {determinism}**",
        f"**N vs P style pair: N={results[3]['prompt_tokens']} tok, P={results[4]['prompt_tokens']} tok**",
    ]
    if results[3].get("mem_first"):
        m = results[3]["mem_first"]
        lines.append(f"**peak RAM at first token (test N): {m['used_gib']:.2f} / {m['total_gib']:.2f} GiB**")
    report = "\n".join(lines) + "\n"

    print("\n" + report)
    with open(args.out, "w") as f:
        f.write(report)
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
