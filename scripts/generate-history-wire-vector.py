#!/usr/bin/env python3
"""Independent JSON wire v1 for an existing native Git-origin framing vector."""
import hashlib
import json
import pathlib
root=pathlib.Path(__file__).resolve().parents[1]
rows=[line.split() for line in (root/'crates/core/tests/fixtures/git-origin-v2.txt').read_text().splitlines()]
rev='native:sha256:'+rows[5][0]
request=dict(version=1,tenant='example',repository='11111111-1111-4111-8111-111111111111',authority='standalone',epoch=3,query=dict(kind='show',revision=rev,commit=rev))
raw=bytes.fromhex(rows[4][1])[22:]
git=hashlib.sha1(b'commit '+str(len(raw)).encode()+b'\0'+raw).hexdigest()
commit=dict(revision=rev,parents=[],snapshot='sha256:'+rows[1][0],changes='sha256:'+rows[2][0],author_name='Alice',author_email='a@example.invalid',timestamp_seconds=1234567890,timezone_minutes=-330,message='message\n',git_origin=dict(revision='git:sha1:'+git,raw_commit='sha256:'+rows[4][0]))
response=dict(version=1,result=dict(authority='standalone',epoch=3,roots=[rev],history=dict(kind='show',commit=commit)))
for name,value in [('history-read-v1',request),('history-response-v1',response)]:
 (root/f'crates/service/tests/fixtures/{name}.json').write_text(json.dumps(value,sort_keys=True,separators=(',',':'))+'\n')
