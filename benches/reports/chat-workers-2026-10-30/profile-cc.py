#!/usr/bin/env python3
import os
import sys
args = ['-Wl,-dead_strip,-no_exported_symbols' if arg == '-Wl,-dead_strip,-S,-x,-no_exported_symbols' else arg for arg in sys.argv[1:]]
os.execv('/usr/bin/cc', ['/usr/bin/cc', *args])
