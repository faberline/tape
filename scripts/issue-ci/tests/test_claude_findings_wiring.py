"""Offline YAML/identity audit; never executes workflow, model or GitHub calls."""
import copy
import json
from pathlib import Path
import unittest
from test_claude_review_enable_wiring import ROOT, PIN, yaml_data, git_blob

RUN = 'claude-findings-${{ github.event.workflow_run.id }}-${{ github.event.workflow_run.run_attempt }}'
CONDITION = "${{ github.event.workflow_run.event == 'workflow_run' && github.event.workflow_run.conclusion == 'success' && github.event.workflow_run.head_repository.full_name == github.repository }}"
PERMISSIONS = {'contents':'read','actions':'read','pull-requests':'read','issues':'write'}


def check_findings(policy, workflow):
    assert set(workflow) == {'name','run-name','on','permissions','concurrency','jobs'}
    assert workflow['name'] == 'Claude issue findings'
    assert workflow['run-name'] == RUN
    assert workflow['on'] == {'workflow_run':{'workflows':['Issue CI handoff'],'types':['completed']}}
    assert workflow['permissions'] == {}
    assert workflow['concurrency'] == {'group':RUN,'cancel-in-progress':'false'}
    assert set(workflow['jobs']) == {'publish-findings'}
    job=workflow['jobs']['publish-findings']
    assert set(job) == {'if','runs-on','timeout-minutes','permissions','steps'}
    assert job['if'] == CONDITION and job['runs-on'] == 'ubuntu-latest'
    assert job['timeout-minutes'] == '5' and job['permissions'] == PERMISSIONS
    assert job['steps'] == [dict(name='Verify review and publish linked-issue findings',
                                uses='faberline/aw/.github/actions/claude-findings@'+PIN)]
    source=(ROOT/'.github/workflows/claude-findings.yml').read_bytes()
    assert policy['claude_findings'] == dict(version=1,path='.github/workflows/claude-findings.yml',
                                            definition_sha=git_blob(source),job='publish-findings')


class FindingsWiringTests(unittest.TestCase):
    def setUp(self):
        self.policy=json.loads((ROOT/'.github/issue-ci-required-gates.json').read_text())
        self.workflow=yaml_data((ROOT/'.github/workflows/claude-findings.yml').read_text())
    def test_exact_offline_wiring(self):
        check_findings(self.policy,self.workflow)
    def test_permission_credential_destination_and_concurrency_mutations_refuse(self):
        mutations=[
            lambda w:w.update(permissions={'contents':'write'}),
            lambda w:w['concurrency'].update(group='shared-across-all-review-heads'),
            lambda w:w['concurrency'].update({'cancel-in-progress':'true'}),
            lambda w:w.update({'run-name':'unbound-review'}),
            lambda w:w['on'].update(workflow_dispatch={}),
            lambda w:w['on']['workflow_run'].update(workflows=['gke-test']),
            lambda w:w['jobs']['publish-findings'].update({'if':'${{ true }}'}),
            lambda w:w['jobs']['publish-findings'].update({'timeout-minutes':'60'}),
            lambda w:w['jobs']['publish-findings'].update(env={'CLAUDE_CODE_OAUTH_TOKEN':'${{ secrets.CLAUDE_CODE_OAUTH_TOKEN }}'}),
            lambda w:w['jobs']['publish-findings']['permissions'].update({'contents':'write'}),
            lambda w:w['jobs']['publish-findings']['permissions'].update({'id-token':'write'}),
            lambda w:w['jobs']['publish-findings']['steps'].append({'uses':'actions/checkout@main'}),
            lambda w:w['jobs']['publish-findings']['steps'][0].update(uses='faberline/aw/.github/actions/claude-findings@main'),
            lambda w:w['jobs']['publish-findings']['steps'][0].update(with_={'issue':'model-selected'}),
        ]
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                value=copy.deepcopy(self.workflow); mutation(value)
                with self.assertRaises(AssertionError): check_findings(self.policy,value)
    def test_policy_destination_or_blob_cannot_override_trusted_controller(self):
        for key,value in (('path','.github/workflows/arbitrary.yml'),('definition_sha','f'*40),('job','other')):
            policy=copy.deepcopy(self.policy); policy['claude_findings'][key]=value
            with self.assertRaises(AssertionError): check_findings(policy,self.workflow)
    def test_model_jobs_stay_read_only_merge_stays_off(self):
        review=yaml_data((ROOT/'.github/workflows/issue-ci.yml').read_text())
        for key in ('claude-preflight','claude-final-review'):
            self.assertEqual(review['jobs'][key]['permissions']['issues'],'read')
        # Existing enable audit independently checks merge false and all gates.
        self.assertIsNone(self.policy['claude_final_review']['merge_producer'])
        self.assertIsNone(self.policy['claude_final_review']['merge_method'])

if __name__=='__main__': unittest.main()
