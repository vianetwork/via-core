'use strict';

// Adapt vLLM Gaudi's last-label-applier lookup: live PR metadata, admin only, no actor shortcut.
// https://github.com/vllm-project/vllm-gaudi/blob/7d290570eebe5396e40708b572843aeea3516f00/.github/workflows/pre-merge-trigger.yaml
exports.readDeferral = async ({github, context, core}) => {
  const raw = context.rawExit;
  const result = {deferred: false};
  if (raw !== 1) return result;
  try {
    const issue_number = context.payload.pull_request.number;
    const {data: pr} = await github.rest.pulls.get({...context.repo, pull_number: issue_number});
    const reason = pr.body?.match(/^Duplication-deferred:[ \t]*(\S[^\r\n]*)$/mi)?.[1].trim();
    if (!reason || !pr.labels.some(l => l.name === 'duplication-deferred')) return result;
    const events = await github.paginate(github.rest.issues.listEventsForTimeline,
      {...context.repo, issue_number, per_page: 100});
    const latest = events.filter(e => ['labeled', 'unlabeled'].includes(e.event) &&
      e.label?.name === 'duplication-deferred').sort((a, b) => b.id - a.id)[0];
    if (latest?.event !== 'labeled' || !latest.actor?.login) return result;
    const {data} = await github.rest.repos.getCollaboratorPermissionLevel(
      {...context.repo, username: latest.actor.login});
    if (data.permission !== 'admin') return result;
    const escaped = reason.replace(/[&<>`|\[\]*_\\]/g, c => `&#${c.codePointAt(0)};`);
    return {deferred: true, actor: latest.actor.login, reason: escaped};
  } catch {
    core.warning('Deferral could not be verified from the current API state.');
    return result;
  }
};
