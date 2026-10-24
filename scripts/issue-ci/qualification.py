"""Trusted, versioned PR applicability policy. No shell, artifacts or PR config."""
import copy
import re

CATEGORIES = {'unit', 'functional_e2e', 'performance'}
KINDS = CATEGORIES | {'policy'}
STATUSES = {'added', 'removed', 'modified', 'renamed', 'copied', 'changed', 'unchanged'}


def require(condition, message):
    if not condition:
        raise RuntimeError('qualification policy: ' + message)


def strings(value):
    return isinstance(value, list) and all(isinstance(x, str) and x.strip() for x in value)


def path_valid(path):
    return (isinstance(path, str) and bool(path) and not path.startswith('/')
            and '\\' not in path and '\x00' not in path
            and not any(part in ('', '.', '..') for part in path.split('/')))


def matches(path, pattern):
    # * and ? stay in a directory; ** crosses directories. **/ includes root.
    parts = []
    index = 0
    while index < len(pattern):
        if pattern[index:index + 3] == '**/':
            parts.append('(?:.*/)?')
            index += 3
        elif pattern[index:index + 2] == '**':
            parts.append('.*')
            index += 2
        elif pattern[index] == '*':
            parts.append('[^/]*')
            index += 1
        elif pattern[index] == '?':
            parts.append('[^/]')
            index += 1
        else:
            parts.append(re.escape(pattern[index]))
            index += 1
    return re.fullmatch(''.join(parts), path) is not None


def validate(config):
    policy = config['qualification']
    require(isinstance(policy, dict) and policy.get('version') == 1
            and type(policy.get('version')) is int, 'version must be 1')
    checks = policy.get('checks')
    require(isinstance(checks, dict) and bool(checks), 'checks must be nonempty')
    allowed = {w['path']: set(w['jobs']) for w in config['workflows']}
    for name, check in checks.items():
        require(isinstance(name, str) and re.fullmatch(r'[A-Za-z0-9_.-]+', name)
                and isinstance(check, dict), 'invalid check ID or object')
        require(check.get('kind') in KINDS, 'unknown check category')
        require(check.get('workflow') in allowed and isinstance(check.get('job'), str)
                and check['job'] in allowed[check['workflow']], 'check outside workflow/job allowlist')
    identities = policy.get('trusted_workflows')
    require(isinstance(identities, dict) and set(identities) == set(allowed),
            'every allowlisted workflow needs a trusted identity')
    for identity in identities.values():
        require(isinstance(identity, dict) and type(identity.get('id')) is int
                and identity['id'] > 0 and isinstance(identity.get('definition_sha'), str)
                and re.fullmatch('[0-9a-f]{40}', identity['definition_sha']),
                'workflow identity needs numeric ID and reviewed definition blob SHA')
    always = policy.get('always')
    require(strings(always) and bool(always) and len(set(always)) == len(always)
            and all(name in checks and checks[name]['kind'] == 'policy' for name in always),
            'always must name real policy checks; zero-check qualification is forbidden')
    protected = policy.get('protected_paths')
    require(strings(protected) and bool(protected) and all(path_valid(p) for p in protected),
            'declare gate infrastructure protected_paths')
    rules = policy.get('rules')
    require(isinstance(rules, list) and bool(rules), 'rules must be nonempty')
    for rule in rules:
        require(isinstance(rule, dict), 'rule must be an object')
        require(strings(rule.get('paths')) and bool(rule['paths'])
                and all(path_valid(p) for p in rule['paths']), 'rule needs safe path globs')
        required, exempt = rule.get('require'), rule.get('exempt')
        require(strings(required) and len(set(required)) == len(required)
                and all(name in checks for name in required), 'rule references unknown checks')
        require(isinstance(exempt, dict) and set(exempt) <= CATEGORIES
                and all(isinstance(reason, str) and reason.strip() for reason in exempt.values()),
                'exemptions need category and reviewed reason')
        kinds = {checks[name]['kind'] for name in required}
        require(not kinds & set(exempt), 'same rule cannot require and exempt a category')
    return policy


def changed_paths(pr, files):
    """GitHub truncates this endpoint at 3000 files. Unknown is never empty."""
    count = pr.get('changed_files')
    issues, paths, names = [], [], set()
    if type(count) is not int or count <= 0 or count >= 3000 or len(files) != count:
        issues.append('incomplete, empty or capped PR files inventory')
    for file in files:
        name = file.get('filename')
        if not path_valid(name) or name in names or file.get('status') not in STATUSES:
            issues.append('invalid, duplicate or unknown PR file metadata')
            continue
        names.add(name)
        paths.append(name)
        if file['status'] in ('renamed', 'copied'):
            previous = file.get('previous_filename')
            if not path_valid(previous):
                issues.append('rename/copy missing previous filename')
            else:
                paths.append(previous)
    return sorted(set(paths)), issues


def select(config, pr, files):
    policy = validate(config)
    paths, issues = changed_paths(pr, files)
    selected = set(policy['always'])
    exemptions = {}
    for path in paths:
        if (path.startswith(('.github/workflows/', '.github/actions/'))
                or any(matches(path, p) for p in policy['protected_paths'])):
            issues.append('gate infrastructure change requires reviewed policy update: ' + path)
        rules = [r for r in policy['rules'] if any(matches(path, p) for p in r['paths'])]
        if not rules:
            issues.append('unmapped changed path: ' + path)
            continue
        required = {name for rule in rules for name in rule['require']}
        selected.update(required)
        covered = {policy['checks'][name]['kind'] for name in required}
        exempt = {kind: reason for rule in rules for kind, reason in rule['exempt'].items()}
        # Every path must independently cover every category. Required wins
        # across overlapping rules; a docs exemption cannot cover another path.
        missing = CATEGORIES - covered - set(exempt)
        if missing:
            issues.append(path + ': no applicability decision for ' + ', '.join(sorted(missing)))
        exemptions[path] = {kind: reason for kind, reason in exempt.items() if kind not in covered}
    wanted = {}
    for name in sorted(selected):
        check = policy['checks'][name]
        wanted.setdefault(check['workflow'], set()).add(check['job'])
    workflows = [dict(path=path, jobs=sorted(jobs)) for path, jobs in sorted(wanted.items())]
    base = pr.get('base', {}).get('sha')
    if not isinstance(base, str) or not re.fullmatch('[0-9a-f]{40}', base):
        issues.append('missing frozen PR base SHA')
    return dict(workflows=workflows, selected=sorted(selected), paths=paths,
                base=base, exemptions=exemptions, issues=sorted(set(issues)))


def trusted_run(run, config):
    context = config['_qualification_context']
    identity = config['qualification']['trusted_workflows'][run['path']]
    if (run.get('workflow_id') != identity['id'] or type(run.get('run_attempt')) is not int
            or run['run_attempt'] < 1):
        return False
    return any(pr.get('number') == context['pr']
               and pr.get('head', {}).get('sha') == context['head']
               and pr.get('base', {}).get('sha') == context['base']
               for pr in run.get('pull_requests', []))


def scoped_config(config, pr, files, definitions):
    """definitions maps (workflow path, immutable ref) to GitHub blob metadata."""
    plan = select(config, pr, files)
    head = pr['head']['sha']
    untrusted = []
    for workflow in plan['workflows']:
        path = workflow['path']
        expected = config['qualification']['trusted_workflows'][path]['definition_sha']
        if any(definitions.get((path, ref), {}).get('sha') != expected
               for ref in (head, plan['base'])):
            untrusted.append(path)
            plan['issues'].append('untrusted head/base workflow definition: ' + path)
    result = copy.deepcopy(config)
    result['workflows'] = plan['workflows']
    result['_qualification_context'] = dict(plan, head=head, pr=pr['number'],
                                             untrusted_workflows=untrusted)
    return result
