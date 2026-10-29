"""Offline YAML/identity audit; never executes workflow, model or GitHub calls."""
import copy
import json
from pathlib import Path
import unittest
from test_claude_review_enable_wiring import ROOT, PIN, yaml_data, git_blob

RUN = 'claude-findings-${{ github.event.workflow_run.id }}-${{ github.event.workflow_run.run_attempt }}'
CONDITION = "${{ github.event.workflow_run.event == 'workflow_run' && (github.event.workflow_run.conclusion == 'success' || github.event.workflow_run.conclusion == 'failure') && github.event.workflow_run.head_repository.full_name == github.repository }}"
LIVE_PIN = '8e2e93e5ba8feb30b6668de39139980ea3bb085c'
PERMISSIONS = {'contents':'read','actions':'read','pull-requests':'read','issues':'write'}


def check_findings(policy, workflow):
    assert set(workflow) == {'name','run-name','on','permissions','concurrency','jobs'}
    assert workflow['name'] == 'Claude issue findings'
    assert workflow['run-name'] == RUN
    assert workflow['on'] == {'workflow_run':{'workflows':['Claude final review'],'types':['completed']}}
    assert workflow['permissions'] == {}
    assert workflow['concurrency'] == {'group':RUN,'cancel-in-progress':'false'}
    assert set(workflow['jobs']) == {'publish-findings'}
    job=workflow['jobs']['publish-findings']
    assert set(job) == {'if','runs-on','timeout-minutes','permissions','steps'}
    assert job['if'] == CONDITION and job['runs-on'] == 'ubuntu-latest'
    assert job['timeout-minutes'] == '5' and job['permissions'] == PERMISSIONS
    assert job['steps'] == [dict(name='Verify review and publish linked-issue findings',
                                uses='faberline/aw/.github/actions/claude-findings@'+LIVE_PIN)]
    source=(ROOT/'.github/workflows/claude-findings.yml').read_bytes()
    assert policy['claude_findings'] == dict(version=1,path='.github/workflows/claude-findings.yml',
                                            definition_sha=git_blob(source),job='publish-findings')


def check_live_review(review):
    readonly={'contents':'read','actions':'read','pull-requests':'read','issues':'read'}
    assert review['permissions']==readonly and set(review['jobs'])=={'preflight','review'}
    preflight=review['jobs']['preflight']; final=review['jobs']['review']
    assert preflight.get('permissions',readonly)==readonly and final.get('permissions',readonly)==readonly
    assert preflight['if']=="github.event.workflow_run.event == 'pull_request' && github.event.workflow_run.conclusion == 'success' && github.event.workflow_run.head_repository.full_name == github.repository"
    assert preflight['steps'][0]['uses']=='faberline/aw/.github/actions/issue-ci@'+LIVE_PIN
    assert preflight['steps'][0]['with']['operation']=='qualify'
    assert preflight['steps'][1]['uses']=='faberline/aw/.github/actions/claude-review@'+LIVE_PIN
    assert preflight['steps'][1]['with']['operation']=='preflight'
    assert final['needs']=='preflight' and final['if']=="needs.preflight.outputs.eligible == 'true'"
    assert final['steps'][0]['with']['ref']=='${{ github.workflow_sha }}'
    assert final['steps'][0]['with']['persist-credentials']=='false'
    assert final['steps'][-1]['uses']=='faberline/aw/.github/actions/claude-review@'+LIVE_PIN
    assert final['steps'][-1]['with']['operation']=='review'
    credentialed=[step for job in review['jobs'].values() for step in job['steps']
                  if 'CLAUDE_CODE_OAUTH_TOKEN' in step.get('env',{})]
    assert credentialed==[final['steps'][-1]]


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
        review=yaml_data((ROOT/'.github/workflows/claude-final-review.yml').read_text())
        check_live_review(review)
        self.assertIn('steps.preflight.outputs.eligible',review['jobs']['preflight']['outputs']['eligible'])
        # Separate native writer remains subject to all qualification gates.
        self.assertIsNone(self.policy['claude_final_review']['merge_producer'])
        self.assertIsNone(self.policy['claude_final_review']['merge_method'])

    def test_live_review_rejects_mutable_source_credentials_and_missing_preflight(self):
        original=yaml_data((ROOT/'.github/workflows/claude-final-review.yml').read_text())
        mutations=[lambda r:r['permissions'].update(issues='write'),
                   lambda r:r['jobs']['preflight']['steps'][0]['with'].update(operation='handoff'),
                   lambda r:r['jobs']['preflight']['steps'][0].update(uses='faberline/aw/.github/actions/issue-ci@main'),
                   lambda r:r['jobs']['review'].update(needs='other'),
                   lambda r:r['jobs']['review']['steps'][0]['with'].update(ref='${{ github.event.pull_request.head.sha }}'),
                   lambda r:r['jobs']['review']['steps'][0]['with'].update({'persist-credentials':'true'}),
                   lambda r:r['jobs']['review']['steps'][-1].update(uses='faberline/aw/.github/actions/claude-review@main')]
        for mutate in mutations:
            value=copy.deepcopy(original);mutate(value)
            with self.subTest(mutation=mutate),self.assertRaises(AssertionError):check_live_review(value)

if __name__=='__main__': unittest.main()
