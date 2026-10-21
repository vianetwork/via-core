'use strict';
const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {readDeferral} = require('../via-duplication/deferral.cjs');
const {resolvePullRequest, preparePublication} = require('../via-duplication/publisher.cjs');

function fixture() {
  const run = {id: 80, run_attempt: 2, workflow_id: 10, path: '.github/workflows/via-duplication.yml',
    event: 'pull_request', status: 'completed', conclusion: 'failure', repository: {full_name: 'via/core'},
    head_sha: 'abc', head_branch: 'feature', head_repository: {id: 4, full_name: 'fork/core', owner: {login: 'fork'}}};
  const pr = {number: 19, state: 'open', body: 'Duplication-deferred: urgent fix', labels: [{name: 'duplication-deferred'}],
    base: {repo: {full_name: 'via/core'}}, head: {sha: 'abc', ref: 'feature', repo: {id: 4}}};
  const state = {run, prs: [pr], runs: [run], jobs: [{name: 'Via Duplication', status: 'completed', conclusion: 'success'},
    {name: 'Via Duplication Boundaries (advisory)', status: 'completed', conclusion: 'failure'},
    {name: 'Via Duplication Tool Tests', status: 'completed', conclusion: 'failure'}], artifacts: [],
    permission: 'admin', events: [{id: 1, event: 'labeled', label: {name: 'duplication-deferred'}, actor: {login: 'maintainer'}}]};
  const outputs = {}; const notices = [];
  const rest = {pulls: {get: async () => ({data: pr}), list: 'prs'},
    issues: {listEventsForTimeline: 'events'}, repos: {getCollaboratorPermissionLevel: async () => ({data: {permission: state.permission}})},
    actions: {getWorkflow: async () => ({data: {id: 10, path: '.github/workflows/via-duplication.yml'}}), getWorkflowRun: async () => ({data: state.run}), listWorkflowRuns: 'runs', listJobsForWorkflowRunAttempt: 'jobs', listWorkflowRunArtifacts: 'artifacts'}};
  const context = {repo: {owner: 'via', repo: 'core'}, serverUrl: 'https://github.com', rawExit: 1,
    payload: {pull_request: {number: 19, body: 'STALE', labels: []}, workflow_run: {id: 80, run_attempt: 2}}};
  const core = {notice(message) { notices.push(message); }, warning() {}, setOutput(k, v) { outputs[k] = v; }};
  return {state, outputs, notices, github: {rest, paginate: async endpoint => state[endpoint]}, context, core};
}

test('deferral reads live body/labels and latest paginated applier, requiring current admin', async () => {
  const f = fixture();
  assert.deepEqual(await readDeferral(f), {deferred: true, actor: 'maintainer', reason: 'urgent fix'});
  for (const permission of ['write', 'triage', 'read', 'unknown']) {
    f.state.permission = permission; assert.equal((await readDeferral(f)).deferred, false);
  }
  f.state.permission = 'admin';
  f.state.events.push({id: 2, event: 'unlabeled', label: {name: 'duplication-deferred'}});
  assert.equal((await readDeferral(f)).deferred, false);
  f.state.events.push({id: 3, event: 'labeled', label: {name: 'duplication-deferred'}, actor: {login: 'triager'}});
  f.state.permission = 'triage'; assert.equal((await readDeferral(f)).deferred, false);
});
test('no event, blank/removed body reason, removed label, API errors and tool errors cannot waive', async () => {
  for (const mutate of [f => f.state.events = [], f => f.state.prs[0].body = 'Duplication-deferred:   ',
    f => f.state.prs[0].body = '', f => f.state.prs[0].labels = [],
    f => f.github.rest.pulls.get = async () => { throw Error('unavailable'); },
    f => f.context.rawExit = 2, f => f.context.rawExit = 0, f => f.context.rawExit = NaN]) {
    const f = fixture(); mutate(f); assert.equal((await readDeferral(f)).deferred, false);
  }
  for (const event of ['opened', 'synchronize', 'reopened', 'ready_for_review', 'edited', 'labeled', 'unlabeled']) {
    const f = fixture(); f.context.payload.action = event; assert.equal((await readDeferral(f)).deferred, true);
  }
});
test('identity handles forks with no payload PR list, and same-repository PRs', async () => {
  const f = fixture(); assert.equal((await resolvePullRequest(f)).number, 19);
  f.state.run.head_repository.full_name = 'via/core'; f.state.run.head_repository.owner.login = 'via';
  assert.equal((await resolvePullRequest(f)).number, 19);
});
test('skip stale or unavailable PR identity and run/attempt with a notice', async () => {
  for (const mutate of [f => f.state.prs.push({...f.state.prs[0], number: 20}),
    f => f.state.prs[0].state = 'closed', f => f.state.prs[0].head.sha = 'new',
    f => f.state.prs[0].head.repo.id = 99, f => f.state.prs[0].base.repo.full_name = 'other/core',
    f => f.state.run.run_attempt = 3, f => f.state.run.status = 'in_progress',
    f => f.state.runs = [], f => f.state.runs.push({...f.state.run, id: 81}),
    f => f.state.runs.push({...f.state.run, run_attempt: 3})]) {
    const f = fixture(); mutate(f); assert.equal(await resolvePullRequest(f), null); assert.deepEqual(f.outputs, {}); assert.equal(f.notices.length, 1);
  }
});
test('wrong producer, invalid identity and API errors remain failures', async () => {
  for (const mutate of [f => f.state.run.path = '.github/workflows/other.yml',
    f => f.state.run.event = 'push', f => f.state.run.workflow_id = 99,
    f => f.state.prs[0].number = -1,
    f => f.github.rest.actions.getWorkflowRun = async () => { throw Error('API unavailable'); }]) {
    const f = fixture(); mutate(f); await assert.rejects(resolvePullRequest(f));
  }
});
test('routine publisher skips never enable the sticky action, including a stale final recheck', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'via-publisher-test-')); const prior = process.env.RUNNER_TEMP;
  process.env.RUNNER_TEMP = root;
  try {
    const early = fixture(); early.state.prs = [];
    assert.equal(await preparePublication(early), null); assert.deepEqual(early.outputs, {publish: 'false'});
    const late = fixture(); let calls = 0;
    late.github.rest.actions.getWorkflowRun = async () => {
      if (++calls === 2) late.state.prs[0].head.sha = 'changed';
      return {data: late.state.run};
    };
    assert.equal(await preparePublication(late), null); assert.deepEqual(late.outputs, {publish: 'false'});
    assert.equal(late.notices.length, 1);
  } finally { if (prior === undefined) delete process.env.RUNNER_TEMP; else process.env.RUNNER_TEMP = prior; fs.rmSync(root, {recursive: true}); }
});
test('trusted required-job prefix, absent report notice and repeat preparation', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'via-publisher-test-')); const prior = process.env.RUNNER_TEMP;
  process.env.RUNNER_TEMP = root;
  try {
    const f = fixture();
    for (let i = 0; i < 2; i++) {
      const prepared = await preparePublication(f); const text = fs.readFileSync(prepared.path, 'utf8');
      assert.equal(prepared.number, 19); assert.equal(f.outputs.publish, 'true'); assert.match(text, /^Required check: \*\*success\*\*/);
      assert.match(text, /report unavailable/); assert.doesNotMatch(text, /Required check: \*\*failure/);
    }
  } finally { if (prior === undefined) delete process.env.RUNNER_TEMP; else process.env.RUNNER_TEMP = prior; fs.rmSync(root, {recursive: true}); }
});
test('artifact Markdown stays data; oversized, symlinked, missing and malformed reports are refused', async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'via-publisher-test-')); const prior = process.env.RUNNER_TEMP;
  process.env.RUNNER_TEMP = root;
  try {
    const f = fixture(); f.state.artifacts = [{name: 'via-duplication-80-2', expired: false, size_in_bytes: 100}];
    const dir = path.join(root, 'via-duplication-artifact'); fs.mkdirSync(dir); const input = path.join(dir, 'body.md');
    await assert.rejects(preparePublication(f), /not downloaded/);
    const hostile = "claimed success; ' ` $(touch /no) ${{github.token}}\n";
    fs.writeFileSync(input, hostile); f.state.jobs[0].conclusion = 'failure';
    const prepared = await preparePublication(f); const body = fs.readFileSync(prepared.path, 'utf8');
    assert.match(body, /^Required check: \*\*failure\*\*/); assert.ok(body.endsWith(hostile));
    assert.equal(f.outputs.number, '19');
    fs.writeFileSync(input, 'x'.repeat(60000)); assert.match(fs.readFileSync((await preparePublication(f)).path, 'utf8'), /Report truncated/);
    fs.writeFileSync(input, 'x'.repeat(2000001)); await assert.rejects(preparePublication(f), /type\/size/);
    fs.unlinkSync(input); fs.symlinkSync(path.join(root, 'missing'), input);
    await assert.rejects(preparePublication(f)); fs.unlinkSync(input); fs.mkdirSync(input);
    await assert.rejects(preparePublication(f), /type\/size/);
    f.state.artifacts[0].size_in_bytes = 2000001; await assert.rejects(preparePublication(f), /size bound/);
    f.state.jobs = []; await assert.rejects(preparePublication(f), /conclusion unavailable/);
  } finally { if (prior === undefined) delete process.env.RUNNER_TEMP; else process.env.RUNNER_TEMP = prior; fs.rmSync(root, {recursive: true}); }
});
