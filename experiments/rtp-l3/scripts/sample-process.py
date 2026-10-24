#!/usr/bin/env python3
"""Sample one task-owned Linux PID; redirect output to a private artifact file."""
import json
import os
import pathlib
import re
import sys
import time

pid = sys.argv[1]
hz = os.sysconf('SC_CLK_TCK')
while True:
    try:
        fields = pathlib.Path('/proc',pid,'stat').read_text().rsplit(')',1)[1].split()
        rss = pathlib.Path('/proc',pid,'status').read_text()
    except FileNotFoundError:
        break
    print(json.dumps({'epoch':time.time(), 'cpu_seconds':(int(fields[11])+int(fields[12]))/hz,
                      'rss_bytes':int(re.search(r'VmRSS:\s+(\d+)',rss)[1])*1024}),flush=True)
    time.sleep(1)
