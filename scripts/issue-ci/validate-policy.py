#!/usr/bin/env python3
"""Offline validation of a reviewed policy; this never qualifies PR evidence."""
from pathlib import Path
import importlib.util
import sys

if __name__ == '__main__':
    spec = importlib.util.spec_from_file_location('issue_ci_handoff', Path(__file__).with_name('handoff.py'))
    ci = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(ci)
    ci.validate_adoption(ci.load_config(sys.argv[1]))
    print('Policy schema valid; this is not a PR qualification result.')
