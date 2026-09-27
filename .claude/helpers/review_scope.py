#!/usr/bin/env python3
"""Read-only candidate enumeration. Recorded commits are evidence, never authorship proof."""
import argparse
import json
import os
from pathlib import Path, PurePosixPath
import subprocess
import sys


def git(repo, *args):
    result = subprocess.run(
        ['git', '--no-optional-locks', '-C', str(repo), *args],
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
        env={**os.environ, 'GIT_OPTIONAL_LOCKS': '0'},
    )
    if result.returncode:
        # Do not echo arbitrary configuration, paths or Git diagnostic contents.
        raise ValueError(f'Git {args[0]} failed (exit {result.returncode})')
    return result.stdout.decode('utf-8', errors='surrogateescape')


def names(repo, *args):
    return sorted(set(filter(None, git(repo, *args).split('\0'))))


def commit(repo, value):
    if not isinstance(value, str) or not value or value.startswith('-'):
        raise ValueError('commit must be a non-option revision')
    return git(repo, 'rev-parse', '--verify', '--end-of-options', value + '^{commit}').strip()


def paths(values, label):
    if not isinstance(values, list) or not all(isinstance(p, str) for p in values):
        raise ValueError(f'{label} must be a list of repo-relative paths')
    for p in values:
        if not p or p.startswith(('/', ':')) or '..' in PurePosixPath(p).parts:
            raise ValueError(f'{label} contains a non-relative or pathspec path')
    return sorted(set(str(PurePosixPath(p)) for p in values))


def related(a, b):
    return a == '.' or b == '.' or a == b or a.startswith(b + '/') or b.startswith(a + '/')


def enumerate_scope(repo, note):
    base = commit(repo, note['base'])
    head = commit(repo, 'HEAD')
    intended = paths(note['paths'], 'paths')
    if not intended:
        raise ValueError('at least one intended path is required')
    if git(repo, 'merge-base', base, head).strip() != base:
        raise ValueError('base must be an ancestor of HEAD; reconcile the task boundary')
    owned_input = note.get('owned_commits', [])
    if not isinstance(owned_input, list):
        raise ValueError('owned_commits must be a list')
    owned = {commit(repo, value) for value in owned_input}
    since = git(repo, 'rev-list', '--reverse', f'{base}..{head}').splitlines()
    if owned - set(since):
        raise ValueError('recorded commit is outside base..HEAD; reconcile the note')
    initial = note.get('initial_state')
    initial_known = isinstance(initial, dict) and all(k in initial for k in ('staged', 'unstaged', 'untracked'))
    initial_paths = []
    if isinstance(initial, dict):
        for key in ('staged', 'unstaged', 'untracked'):
            if key in initial:
                initial_paths += paths(initial[key], 'initial_state.' + key)
    committed = []
    for sha in since:
        # -m includes all parent diffs for merge commits; never silently omit merge changes.
        changed = names(repo, 'diff-tree', '--root', '-m', '--no-commit-id', '--name-only',
                        '-r', '-z', '--no-renames', sha)
        committed.append({'commit': sha, 'recorded_owned': sha in owned, 'paths': changed})
    current = {
        'staged': names(repo, 'diff', '--cached', '--name-only', '-z', '--no-renames'),
        'unstaged': names(repo, 'diff', '--name-only', '-z', '--no-renames'),
        'untracked': names(repo, 'ls-files', '--others', '--exclude-standard', '-z'),
    }
    candidate_paths = set(p for row in committed if row['recorded_owned'] for p in row['paths'])
    candidate_paths.update(p for group in current.values() for p in group if any(related(p, q) for q in intended))
    unattributed = [row for row in committed if not row['recorded_owned']]
    return {
        'base': base, 'head': head, 'paths': intended,
        'committed': committed, 'current': current,
        'candidate_paths': sorted(candidate_paths),
        'recorded_outside_scope': sorted(p for p in candidate_paths if not any(related(p, q) for q in intended)),
        'initial_evidence_missing': not initial_known,
        'initial_overlaps': sorted(p for p in set(initial_paths) if any(related(p, q) for q in intended)),
        'unattributed_commits': [row['commit'] for row in unattributed],
        'unattributed_overlaps': sorted(set(p for row in unattributed for p in row['paths'] if any(related(p, q) for q in intended))),
        'ownership_proven': False,
        'coverage': 'Candidates only; reconcile attribution and inspect diffs before claiming complete review.',
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', default='.')
    parser.add_argument('--note', type=Path)
    parser.add_argument('--base')
    parser.add_argument('--path', action='append')
    parser.add_argument('--owned-commit', action='append')
    args = parser.parse_args()
    try:
        note = json.loads(args.note.read_text()) if args.note else {}
        if not isinstance(note, dict):
            raise ValueError('note must be a JSON object')
        for key, value in [('base', args.base), ('paths', args.path), ('owned_commits', args.owned_commit)]:
            if value is not None:
                note[key] = value
        if 'base' not in note or 'paths' not in note:
            raise ValueError('explicit base and intended paths are required')
        print(json.dumps(enumerate_scope(args.repo, note), indent=2, ensure_ascii=True))
    except (ValueError, OSError, KeyError, TypeError) as exc:
        print(f'review scope: {exc}', file=sys.stderr)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main())
