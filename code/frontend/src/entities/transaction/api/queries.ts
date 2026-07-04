import { useMutation, useQuery } from '@tanstack/react-query'

import {
  fetchJobStatus,
  fetchTransactionGraph,
  ingestWallet,
  type FetchGraphPayload,
} from '@/entities/transaction/api/transaction-graph'

export const transactionGraphQueryKeys = {
  all: ['transaction-graph'] as const,
  job: (jobId: string) => [...transactionGraphQueryKeys.all, 'job', jobId] as const,
  graph: (payload: FetchGraphPayload) => [...transactionGraphQueryKeys.all, 'graph', payload] as const,
}

const ACTIVE_JOB_STATUSES = new Set(['pending', 'running'])

export function useIngestWalletMutation() {
  return useMutation({
    mutationFn: ingestWallet,
  })
}

export function useFetchTransactionGraphMutation() {
  return useMutation({
    mutationFn: fetchTransactionGraph,
  })
}

export function useIngestJobStatusQuery(jobId: string | null) {
  return useQuery({
    queryKey: transactionGraphQueryKeys.job(jobId ?? 'none'),
    queryFn: () => fetchJobStatus(jobId!),
    enabled: Boolean(jobId),
    refetchInterval: (query) => {
      const status = query.state.data?.status.toLowerCase()
      if (!status || !ACTIVE_JOB_STATUSES.has(status)) {
        return false
      }
      return 2000
    },
  })
}
