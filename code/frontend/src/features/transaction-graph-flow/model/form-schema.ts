import { z } from 'zod'

export const graphFlowFormSchema = z.object({
  address: z.string().trim().min(1, 'Field address is required.'),
  fromBlock: z
    .string()
    .trim()
    .min(1, 'Field from_block is required.')
    .refine((value) => Number.isInteger(Number(value)) && Number(value) >= 0, {
      message: 'Field from_block must be a non-negative integer.',
    }),
  maxDepth: z.string().trim(),
  maxNodes: z.string().trim(),
})

export type GraphFlowFormValues = z.infer<typeof graphFlowFormSchema>

export function graphFlowFormToPayload(values: GraphFlowFormValues) {
  const maxDepth = values.maxDepth.trim() ? Number(values.maxDepth) : 3
  const maxNodes = values.maxNodes.trim() ? Number(values.maxNodes) : 500

  return {
    address: values.address.trim(),
    from_block: Number(values.fromBlock),
    max_depth: Number.isInteger(maxDepth) && maxDepth >= 0 ? maxDepth : 3,
    max_nodes: Number.isInteger(maxNodes) && maxNodes > 0 ? maxNodes : 500,
  }
}
