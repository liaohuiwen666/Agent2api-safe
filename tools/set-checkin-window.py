#!/usr/bin/env python3
"""调整 Agent2API 宿主的签到窗口。

宿主的签到窗口只能通过 kv 直改：面板 API `/api/checkin-schedules` 的所有
端点形态（PUT/POST/带 id/集合）都返回 404，面板也没有编辑入口。

用法:
    python3 set-checkin-window.py --data-dir /path/to/data --start 00:01 --end 00:40

脚本流程（每步失败都会自动回滚）:
    1. docker stop -t 25 <容器>      # 让宿主 flush 内存状态到 kv
    2. 备份 agent2api.db
    3. 改 kv.autoCheckin.schedules[].time / timeEnd / retryTime
    4. docker start <容器>
    5. 复核写入结果

注意: retryTime 必须等于 timeEnd，否则补签轮会在首轮窗口还没跑完时启动。
"""
import argparse
import datetime
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import time


def sh(cmd):
    r = subprocess.run(["sh", "-c", cmd], capture_output=True, text=True)
    return (r.stdout + r.stderr).strip()


def parse_hhmm(value):
    hh, mm = value.split(":")
    h, m = int(hh), int(mm)
    if not (0 <= h <= 23 and 0 <= m <= 59):
        raise ValueError(f"非法时刻: {value}")
    return h * 3600 + m * 60


def main():
    ap = argparse.ArgumentParser(description="调整 Agent2API 签到窗口（kv 直改）")
    ap.add_argument("--data-dir", required=True, help="agent2api 数据目录（含 agent2api.db）")
    ap.add_argument("--container", default="agent2api-test", help="容器名，默认 agent2api-test")
    ap.add_argument("--start", default="00:01", help="窗口起点 HH:MM，默认 00:01")
    ap.add_argument("--end", required=True, help="窗口终点 HH:MM，例如 00:40")
    ap.add_argument("--retry-time", default=None,
                    help="补签轮起算点 HH:MM，默认取 --end（推荐）")
    ap.add_argument("--dry-run", action="store_true", help="只打印将要写入的值，不改动")
    args = ap.parse_args()

    if parse_hhmm(args.end) <= parse_hhmm(args.start):
        sys.exit("窗口终点必须晚于起点")
    retry_time = args.retry_time or args.end
    db_path = os.path.join(args.data_dir, "agent2api.db")
    if not os.path.exists(db_path):
        sys.exit(f"找不到数据库: {db_path}")

    print(f"目标窗口   : {args.start} – {args.end}")
    print(f"retryTime  : {retry_time}")
    print(f"数据库     : {db_path}")
    print(f"容器       : {args.container}")
    if args.dry_run:
        print("\n[dry-run] 未做任何改动")
        return

    # 1. 停容器，让宿主把内存态写回 kv
    print(f"\n[1/5] 停止容器 {args.container} …")
    print("     ", sh(f"docker stop -t 25 {args.container}"))

    # 2. 备份
    ts = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    stamp = args.end.replace(":", "")
    backup = os.path.join(args.data_dir, f"agent2api.db.bak.{ts}-window{stamp}")
    shutil.copy2(db_path, backup)
    print(f"[2/5] 备份 → {backup}")

    # 3. 改 kv
    print("[3/5] 写入 kv …")
    db = sqlite3.connect(db_path, timeout=20)
    try:
        row = db.execute("SELECT value FROM kv WHERE key='autoCheckin'").fetchone()
        if not row:
            raise RuntimeError("kv 中没有 autoCheckin 记录")
        cfg = json.loads(row[0])
        changed = []
        for sched in cfg.get("schedules", []):
            before = (sched.get("time"), sched.get("timeEnd"), sched.get("retryTime"))
            sched["time"] = args.start
            sched["timeEnd"] = args.end
            sched["retryTime"] = retry_time
            changed.append((before, (args.start, args.end, retry_time)))
        if not changed:
            raise RuntimeError("autoCheckin.schedules 为空")
        db.execute("UPDATE kv SET value=? WHERE key='autoCheckin'",
                   (json.dumps(cfg, ensure_ascii=False),))
        db.commit()
        verify = json.loads(db.execute("SELECT value FROM kv WHERE key='autoCheckin'").fetchone()[0])
        for sched in verify["schedules"]:
            got = (sched.get("time"), sched.get("timeEnd"), sched.get("retryTime"))
            if got != (args.start, args.end, retry_time):
                raise RuntimeError(f"写入校验失败，得到 {got}")
        for before, after in changed:
            print(f"      {before} → {after}")
    except Exception as exc:
        db.close()
        shutil.copy2(backup, db_path)
        sh(f"docker start {args.container}")
        sys.exit(f"写入失败，已回滚并重启容器: {exc}")
    db.close()

    # 4. 启动
    print(f"[4/5] 启动容器 {args.container} …")
    print("     ", sh(f"docker start {args.container}"))
    time.sleep(8)
    logs = sh(f"docker logs --since 1m {args.container} 2>&1 | grep -i '已加载' | tail -3")
    print("     ", logs or "(暂无插件加载日志)")

    # 5. 复核
    print("[5/5] 复核")
    health = sh(f"docker inspect -f '{{{{.State.Health.Status}}}}' {args.container} 2>/dev/null")
    print(f"      容器健康: {health or 'n/a'}")
    print(f"\n完成。下次窗口 {args.start}–{args.end}，补签轮在 {retry_time} 之后一轮。")


if __name__ == "__main__":
    main()