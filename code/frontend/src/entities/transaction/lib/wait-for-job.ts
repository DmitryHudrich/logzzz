import { fetchJobStatus } from '@/entities/transaction/api/transaction-graph'

const ACTIVE_JOB_STATUSES = new Set(['pending', 'running'])

export async function waitForIngestJob(jobId: string, intervalMs = 2000) {
  while (true) {
    const job = await fetchJobStatus(jobId)
    const status = job.status.toLowerCase()

    if (status === 'done') {
      return job
    }

    if (status === 'failed') {
      throw new Error(job.error ?? 'Ingest job failed.')
    }

    if (!ACTIVE_JOB_STATUSES.has(status)) {
      throw new Error(`Unexpected ingest job status: ${job.status}`)
    }

    await new Promise((resolve) => {
      window.setTimeout(resolve, intervalMs)
    })
  }
}
