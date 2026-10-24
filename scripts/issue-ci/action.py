#!/usr/bin/env python3
"""Pinned action entrypoint: fetch only the consumer's default-branch policy."""
import base64
import importlib.util
import json
import os
from pathlib import Path
import re
import sys
import subprocess
import tempfile


def controller():
    spec = importlib.util.spec_from_file_location('issue_ci_handoff', Path(__file__).with_name('handoff.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def trusted_policy(ci, repo, path):
    if not re.fullmatch(r'[\w.-]+/[\w.-]+', repo):
        raise RuntimeError('invalid consumer repository')
    if not re.fullmatch(r'\.github/[\w.-]+\.json', path):
        raise RuntimeError('policy must be a JSON file directly in .github')
    meta = ci.api(f'repos/{repo}')
    branch = meta['default_branch']
    from urllib.parse import quote
    revision = ci.api(f'repos/{repo}/commits/{quote(branch, safe="")}')['sha']
    if not re.fullmatch('[0-9a-f]{40}', revision):
        raise RuntimeError('default branch has no immutable revision')
    data = ci.api(f'repos/{repo}/contents/{path}?ref={revision}')
    if data.get('encoding') != 'base64' or data.get('type') != 'file':
        raise RuntimeError('trusted policy is not a GitHub file')
    content = base64.b64decode(''.join(data['content'].split()), validate=True)
    if len(content) > 262144:
        raise RuntimeError('trusted policy exceeds size limit')
    # Parse before writing. No PR config, artifact or checkout is consulted.
    json.loads(content)
    return content


def main():
    if not re.fullmatch('[0-9a-f]{40}', os.environ.get('AW_ACTION_REF', '')):
        raise RuntimeError('the shared action must use a reviewed immutable 40-hex commit reference')
    ci = controller()
    repo = os.environ['GITHUB_REPOSITORY']
    operation = os.environ.get('INPUT_OPERATION', 'handoff')
    if operation not in ('validate', 'qualify', 'handoff'):
        raise RuntimeError('operation must be validate, qualify or handoff')
    content = trusted_policy(ci, repo, os.environ.get('INPUT_POLICY', '.github/issue-ci-required-gates.json'))
    with tempfile.TemporaryDirectory(prefix='issue-ci-policy-') as tmp:
        path = Path(tmp) / 'policy.json'
        path.write_bytes(content)
        ci.validate_adoption(ci.load_config(path))
        if operation == 'validate':
            print('Trusted default-branch policy validated; no lifecycle mutation.')
            return 0
        args = ['handoff.py', '--repo', repo, '--config', str(path)]
        if operation == 'handoff':
            run = os.environ.get('INPUT_RUN_ID', '')
            if not re.fullmatch('[1-9][0-9]*', run):
                raise RuntimeError('handoff requires a workflow_run ID')
            args += ['--reconcile', '--run-id', run]
        else:
            pr, head = os.environ.get('INPUT_PR', ''), os.environ.get('INPUT_HEAD', '')
            if not re.fullmatch('[1-9][0-9]*', pr) or not re.fullmatch('[0-9a-f]{40}', head):
                raise RuntimeError('qualify requires PR number and exact head SHA')
            args += ['--pr', pr, '--head', head]
        sys.argv = args
        return ci.main()


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (RuntimeError, KeyError, TypeError, ValueError, OSError, subprocess.CalledProcessError) as exc:
        print(f'refused: {exc}', file=sys.stderr)
        sys.exit(2)
