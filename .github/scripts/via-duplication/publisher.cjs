'use strict';
const fs = require('node:fs');
const path = require('node:path');
const workflowPath = '.github/workflows/via-duplication.yml';
const fullName = context => `${context.repo.owner}/${context.repo.repo}`;

// Adapt API PR lookup and head guard; require current workflow/run/attempt, including fork identity.
// https://github.com/kubeflow/pipelines/blob/1981e0da1993aae596f7cd219d0edf3cdcb26ede/.github/workflows/add-ci-passed-label.yml
exports.resolvePullRequest = async ({github, context, core}) => {
  const skip = message => { core.notice(message); return null; };
  const event = context.payload.workflow_run;
  if (!Number.isSafeInteger(event?.id) || event.id <= 0) throw Error('Invalid run ID');
  const {data: run} = await github.rest.actions.getWorkflowRun({...context.repo, run_id: event.id});
  const {data: workflow} = await github.rest.actions.getWorkflow({...context.repo, workflow_id: 'via-duplication.yml'});
  if (run.workflow_id !== workflow.id || workflow.path !== workflowPath || run.event !== 'pull_request' || run.path !== workflowPath ||
      run.repository.full_name !== fullName(context)) throw Error('Wrong producer');
  if (run.status !== 'completed' || run.run_attempt !== event.run_attempt ||
      !run.head_repository?.full_name || !run.head_branch) return skip('Producer attempt is no longer current or its head is unavailable.');
  const prs = await github.paginate(github.rest.pulls.list, {...context.repo, state: 'open',
    head: `${run.head_repository.owner.login}:${run.head_branch}`, per_page: 100});
  const matches = prs.filter(pr => pr.state === 'open' && pr.base.repo.full_name === fullName(context) &&
    pr.head.repo?.id === run.head_repository.id && pr.head.ref === run.head_branch && pr.head.sha === run.head_sha);
  if (matches.length !== 1) return skip('No unique current open PR for producer head; skipping comment.');
  if (!Number.isSafeInteger(matches[0].number) || matches[0].number <= 0) throw Error('Invalid PR number');
  const runs = await github.paginate(github.rest.actions.listWorkflowRuns, {...context.repo,
    workflow_id: run.workflow_id, event: 'pull_request', branch: run.head_branch, head_sha: run.head_sha, per_page: 100});
  const applicable = runs.filter(r => r.head_repository?.id === run.head_repository.id && r.head_sha === run.head_sha);
  if (!applicable.some(r => r.id === run.id && r.run_attempt === run.run_attempt) || applicable.some(r => r.id > run.id || (r.id === run.id && r.run_attempt > run.run_attempt)))
    return skip('A newer producer run/attempt exists or this run is not current; skipping comment.');
  return {number: matches[0].number, run};
};

// Adapt FastPFOR's two-stage report: artifact data cannot select the PR or executable code.
// https://github.com/fast-pack/FastPFOR-rs/blob/ebca273acf1de7644bd01bcbd79b03c61c1348ed/.github/workflows/pr-comments.yml
exports.preparePublication = async ({github, context, core}) => {
  core.setOutput('publish', 'false');
  const current = await exports.resolvePullRequest({github, context, core});
  if (!current) return null;
  const {number, run} = current;
  const jobs = await github.paginate(github.rest.actions.listJobsForWorkflowRunAttempt,
    {...context.repo, run_id: run.id, attempt_number: run.run_attempt, per_page: 100});
  const required = jobs.filter(j => j.name === 'Via Duplication');
  if (required.length !== 1 || required[0].status !== 'completed' || !required[0].conclusion) throw Error('Required job conclusion unavailable');
  const link = `${context.serverUrl}/${fullName(context)}/actions/runs/${run.id}/attempts/${run.run_attempt}`;
  const directory = fs.mkdtempSync(path.join(process.env.RUNNER_TEMP, 'via-duplication-report-'));
  const artifacts = await github.paginate(github.rest.actions.listWorkflowRunArtifacts,
    {...context.repo, run_id: run.id, per_page: 100});
  const expected = `via-duplication-${run.id}-${run.run_attempt}`;
  const matching = artifacts.filter(a => a.name === expected && !a.expired);
  if (matching.length > 1) throw Error('Ambiguous artifact');
  let body = 'Producer report unavailable; version not resolved. See the run for diagnostics.';
  if (matching.length === 1) {
    if (matching[0].size_in_bytes > 2_000_000) throw Error('Artifact exceeds size bound');
  }
  const input = path.join(process.env.RUNNER_TEMP, 'via-duplication-artifact', 'body.md');
  if (matching.length && fs.existsSync(input)) {
    const parent = fs.lstatSync(path.dirname(input));
    if (!parent.isDirectory() || parent.isSymbolicLink()) throw Error('Invalid artifact directory');
    const stat = fs.lstatSync(input);
    if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 2_000_000) throw Error('Invalid report file type/size');
    body = fs.readFileSync(input, 'utf8');
  } else if (matching.length) {
    throw Error('Expected artifact body was not downloaded');
  }
  if (Buffer.byteLength(body) > 48_000) {
    body = Buffer.from(body).subarray(0, 48_000).toString('utf8').replace(/\uFFFD$/, '');
    body += `\n\nReport truncated. Download the full report from [this run](${link}).`;
  }
  const output = path.join(directory, 'comment.md');
  fs.writeFileSync(output, `Required check: **${required[0].conclusion}** ([run](${link})).\n\nProducer-supplied report follows.\n\n${body}`);
  // Repeat trusted head/latest-run validation after reading data and before the sticky action.
  if (!await exports.resolvePullRequest({github, context, core})) return null;
  core.setOutput('number', String(number)); core.setOutput('path', output); core.setOutput('publish', 'true');
  return {number, path: output};
};
