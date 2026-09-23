#!/usr/bin/env python3
"""Validate the actual schema/static SQL with Python SQLite, not the Rust program.

This is deliberately NOT a substitute for cargo test: it cannot detect Rust type
errors, incorrect params! argument order, IPC bugs, or host adapter behavior.
"""
from pathlib import Path
import json, re, sqlite3, sys

ROOT = Path(__file__).resolve().parents[1]
source = (ROOT / 'src/store.rs').read_text()
conn = sqlite3.connect(':memory:')
conn.execute('PRAGMA foreign_keys=ON')
conn.executescript((ROOT / 'src/schema.sql').read_text())
checks = []

def check(name, condition):
    if not condition:
        raise AssertionError(name)
    checks.append(name)

def extract(prefix):
    for match in re.finditer(r'"((?:[^"\\]|\\.)*)"', source):
        value = json.loads('"' + match.group(1).replace('\n', '\\n') + '"')
        if value.startswith(prefix):
            return value
    raise AssertionError('No literal found for ' + prefix)

# Compile every actual static SQL literal used in the Rust implementation.
statements = []
for match in re.finditer(r'"((?:[^"\\]|\\.)*)"', source):
    value = json.loads('"' + match.group(1).replace('\n', '\\n') + '"')
    if not re.match(r'^(SELECT|INSERT|UPDATE|DELETE)\b', value) or re.search(r'\{[A-Za-z_][^{}]*\}', value) or value.rstrip().endswith('='):
        continue
    # These static statements use either numbered or unnumbered placeholders.
    numbered = [int(x) for x in re.findall(r'\?(\d+)', value)]
    n = max(numbered) if numbered else value.count('?')
    conn.execute('EXPLAIN ' + value, [None] * n).fetchall()
    statements.append(value)
check('static SQL prepares against the actual schema', len(statements) >= 25)

conn.execute("INSERT INTO agents(name,role,topics,joined_ms,last_seen_ms) VALUES('alice','worker','[\"parser\"]',1,1)")
conn.execute("INSERT INTO agents(name,role,topics,joined_ms,last_seen_ms) VALUES('bob','worker','[\"tests\"]',1,1)")
conn.execute("INSERT INTO agents(name,role,topics,joined_ms,last_seen_ms) VALUES('chief','steward','[\"other\"]',1,1)")
post = extract('INSERT INTO cards(kind,topic,title,summary,status,priority,pinned,tags,author,assignee,created_ms,updated_ms)')
conn.execute(post, ['task','parser','Use oldneedle format','Current summary','open',1,0,'["parser"]','alice',None,1,1])
id_ = conn.execute('SELECT last_insert_rowid()').fetchone()[0]
check('FTS indexes current card insertion', conn.execute("SELECT rowid FROM card_fts WHERE card_fts MATCH 'oldneedle'").fetchall() == [(id_,)])
conn.execute("UPDATE cards SET title='Use newneedle format',rev=rev+1 WHERE id=?", [id_])
check('FTS replaces obsolete head text', conn.execute("SELECT count(*) FROM card_fts WHERE card_fts MATCH 'oldneedle'").fetchone()[0] == 0)
check('FTS finds new head text', conn.execute("SELECT rowid FROM card_fts WHERE card_fts MATCH 'newneedle'").fetchall() == [(id_,)])
conn.execute("INSERT INTO events(ts_ms,actor,op,card_id,payload) VALUES(1,'alice','post',?,'{}')", [id_])
seq = conn.execute('SELECT last_insert_rowid()').fetchone()[0]
conn.execute("INSERT INTO event_fts(rowid,text) VALUES(?,'oldneedle')", [seq])
check('history index preserves historical text', conn.execute("SELECT rowid FROM event_fts WHERE event_fts MATCH 'oldneedle'").fetchall() == [(seq,)])

fanout = extract('INSERT INTO deliveries(agent,card_id,pending_seq)')
conn.execute(fanout, [id_,seq,'alice',0,'parser','alice',None,None,False])
check('off-topic ordinary event does not interrupt bob', conn.execute("SELECT count(*) FROM deliveries WHERE agent='bob'").fetchone()[0] == 0)
check('steward receives off-topic activity', conn.execute("SELECT count(*) FROM deliveries WHERE agent='chief'").fetchone()[0] == 1)
conn.execute(fanout, [id_,seq,'alice',0,'parser','alice',None,None,True])
check('new unassigned work is discoverable outside topic filters', conn.execute("SELECT count(*) FROM deliveries WHERE agent='bob'").fetchone()[0] == 1)
conn.execute(fanout, [id_,2,'alice',0,'parser','alice',None,None,False])
check('incidental receipt does not subscribe to later events', conn.execute("SELECT pending_seq FROM deliveries WHERE agent='bob'").fetchone()[0] == seq)
conn.execute("INSERT INTO participants(agent,card_id) VALUES('bob',?)", [id_])
for n in range(2,102):
    conn.execute(fanout, [id_,n,'alice',0,'parser','alice',None,None,False])
check('100 updates coalesce into a single pending row', conn.execute("SELECT count(*),max(pending_seq) FROM deliveries WHERE agent='bob'").fetchone() == (1,101))
ack = extract('UPDATE deliveries SET ack_seq=max(ack_seq,?)')
conn.execute(ack, [50,'bob',id_])
check('acknowledging an older delivery leaves the newer head pending', conn.execute("SELECT pending_seq>ack_seq FROM deliveries WHERE agent='bob'").fetchone()[0] == 1)
conn.execute(ack, [101,'bob',id_])
check('acknowledging through the exact pending sequence clears attention', conn.execute("SELECT pending_seq>ack_seq FROM deliveries WHERE agent='bob'").fetchone()[0] == 0)
conn.execute(ack, [25,'bob',id_])
check('acknowledgment cursor is monotonic', conn.execute("SELECT ack_seq FROM deliveries WHERE agent='bob'").fetchone()[0] == 101)
check('acknowledgment does not close the work item', conn.execute('SELECT status FROM cards WHERE id=?',[id_]).fetchone()[0] == 'open')

question = extract("INSERT INTO cards(kind,topic,title,summary,status,priority,tags,author,assignee,created_ms,updated_ms) VALUES('question'")
conn.execute(question, ['parser','Objection','The contract is incompatible',1,'["parent:1"]','bob','alice',2,2])
check('actionable annotations can create linked durable questions', conn.execute("SELECT kind,assignee FROM cards WHERE id=last_insert_rowid()").fetchone() == ('question','alice'))

# Compile the actual dynamic join seed and verify active-only seeding.
active = re.search(r'const ACTIVE:\s*&str\s*=\s*"([^"]+)"', source).group(1)
relevant = re.search(r'const RELEVANT:\s*&str\s*=\s*"([^"]+)"', source).group(1)
seed = extract('INSERT INTO deliveries(agent,card_id,pending_seq) SELECT ?')
seed = seed.replace('{ACTIVE}',active).replace('{RELEVANT}',relevant)
conn.execute("INSERT INTO agents(name,role,topics,joined_ms,last_seen_ms) VALUES('new','worker','[\"*\"]',3,3)")
conn.execute("INSERT INTO cards(kind,topic,title,summary,status,priority,author,created_ms,updated_ms) VALUES('note','archive','Closed','No need to replay','resolved',3,'alice',1,1)")
conn.execute(seed, ['new']*6)
check('newcomer snapshot excludes closed history', conn.execute("SELECT count(*) FROM deliveries d JOIN cards c ON c.id=d.card_id WHERE d.agent='new' AND c.status='resolved'").fetchone()[0] == 0)
check('newcomer snapshot includes active heads', conn.execute("SELECT count(*) FROM deliveries WHERE agent='new'").fetchone()[0] == 2)

# Test FTS boolean/prefix features used by the CLI.
check('FTS supports prefix and boolean queries', conn.execute("SELECT count(*) FROM card_fts WHERE card_fts MATCH 'newneed* OR incompat*'").fetchone()[0] == 2)
conn.commit()
conn.execute('BEGIN IMMEDIATE')
conn.execute("UPDATE cards SET title='rolledback' WHERE id=?", [id_])
conn.rollback()
check('rollback keeps head and current FTS consistent', conn.execute("SELECT count(*) FROM card_fts WHERE card_fts MATCH 'rolledback'").fetchone()[0] == 0)

conn.execute("INSERT INTO requests(actor,key,request,response) VALUES('alice','retry-key','{}','{}')")
try:
    conn.execute("INSERT INTO requests(actor,key,request,response) VALUES('alice','retry-key','{}','{}')")
    raise AssertionError('duplicate key accepted')
except sqlite3.IntegrityError:
    checks.append('idempotency keys unique per actor')
check('database structural integrity', conn.execute('PRAGMA integrity_check').fetchone()[0] == 'ok')
check('database referential integrity', conn.execute('PRAGMA foreign_key_check').fetchall() == [])

report = {'validation_kind':'schema_and_SQL_only','sqlite_version':sqlite3.sqlite_version,
          'static_statements_prepared':len(statements),'checks_passed':len(checks),'checks':checks,
          'not_tested':['Rust compilation','Rust tests','Rust parameter bindings','IPC runtime','latency','coding-agent host integration']}
print(json.dumps(report, indent=2))
