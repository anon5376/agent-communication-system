/**
 * Change watcher over the events table.
 *
 * An idle waiter never writes. It polls `PRAGMA data_version`, which changes only
 * when another connection commits and costs no disk write, with a bounded
 * backoff (minPollMs doubling up to maxPollMs). An fs.watch on the database
 * directory wakes the loop early when bus.db or bus.db-wal changes. Only when
 * data_version moves does it read max(events.seq), a primary-key lookup.
 */
import { watch } from "node:fs";
import { basename, dirname } from "node:path";
export class ChangeWatcher {
    db;
    dbPath;
    versionStatement;
    seqStatement;
    minPollMs;
    maxPollMs;
    watcher = null;
    wakers = new Set();
    fsEpoch = 0;
    closed = false;
    constructor(db, dbPath, options = {}) {
        this.db = db;
        this.dbPath = dbPath;
        this.minPollMs = Math.max(1, options.minPollMs ?? 10);
        this.maxPollMs = Math.max(this.minPollMs, options.maxPollMs ?? 100);
        this.versionStatement = db.prepare("PRAGMA data_version");
        this.seqStatement = db.prepare("SELECT COALESCE(MAX(seq), 0) AS seq FROM events");
        if (options.fsWatch !== false)
            this.startFsWatch();
    }
    startFsWatch() {
        const name = basename(this.dbPath);
        const interesting = new Set([name, `${name}-wal`]);
        try {
            this.watcher = watch(dirname(this.dbPath), { persistent: false }, (_event, filename) => {
                if (filename && !interesting.has(String(filename)))
                    return;
                this.fsEpoch += 1;
                for (const wake of [...this.wakers])
                    wake();
            });
            this.watcher.on("error", () => { this.watcher?.close(); this.watcher = null; });
            this.watcher.unref?.();
        }
        catch {
            this.watcher = null; // polling alone still bounds latency by maxPollMs
        }
    }
    dataVersion() {
        const row = this.versionStatement.get();
        return Number(row.data_version);
    }
    currentSeq() {
        const row = this.seqStatement.get();
        return Number(row.seq);
    }
    /**
     * Resolve with the latest events.seq once it is greater than `sinceSeq`, or with
     * the unchanged sequence number when `timeoutMs` elapses or `signal` aborts.
     */
    async next(sinceSeq, timeoutMs, signal) {
        if (this.closed)
            throw new Error("change watcher is closed");
        let seq = this.currentSeq();
        if (seq > sinceSeq)
            return seq;
        const deadline = Date.now() + Math.max(0, timeoutMs);
        let version = this.dataVersion();
        let interval = this.minPollMs;
        while (!this.closed && !signal?.aborted) {
            const remaining = deadline - Date.now();
            if (remaining <= 0)
                break;
            const fsEpoch = this.fsEpoch;
            await this.sleep(Math.min(interval, remaining), signal);
            if (this.closed)
                break;
            const current = this.dataVersion();
            if (current !== version) {
                version = current;
                seq = this.currentSeq();
                if (seq > sinceSeq)
                    return seq;
                interval = this.minPollMs;
            }
            else {
                interval = this.fsEpoch === fsEpoch ? Math.min(interval * 2, this.maxPollMs) : this.minPollMs;
            }
        }
        return this.closed ? seq : this.currentSeq();
    }
    sleep(ms, signal) {
        return new Promise((resolve) => {
            let timer = null;
            const done = () => {
                if (timer)
                    clearTimeout(timer);
                this.wakers.delete(done);
                signal?.removeEventListener("abort", done);
                resolve();
            };
            timer = setTimeout(done, ms);
            this.wakers.add(done);
            signal?.addEventListener("abort", done, { once: true });
        });
    }
    close() {
        this.closed = true;
        this.watcher?.close();
        this.watcher = null;
        for (const wake of [...this.wakers])
            wake();
    }
}
//# sourceMappingURL=changes.js.map