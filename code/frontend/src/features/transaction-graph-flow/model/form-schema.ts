import { z } from 'zod'

export const graphFlowFormSchema = z.object({
  address: z.string().trim().min(1, 'Field address is required.'),
  fromBlock: z
    .string()
    .trim()
    .refine((value) => value.length === 0 || (Number.isInteger(Number(value)) && Number(value) >= 0), {
      message: 'Field from_block must be a non-negative integer.',
    }),
  toBlock: z
    .string()
    .trim()
    .refine((value) => value.length === 0 || (Number.isInteger(Number(value)) && Number(value) >= 0), {
      message: 'Field to_block must be a non-negative integer.',
    }),
  maxDepth: z.string().trim(),
  maxNodes: z.string().trim(),
}).superRefine((values, ctx) => {
  if (!values.fromBlock.trim() || !values.toBlock.trim()) {
    return
  }

  const fromBlock = Number(values.fromBlock)
  const toBlock = Number(values.toBlock)
  if (fromBlock > toBlock) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      path: ['toBlock'],
      message: 'Field to_block must be greater than or equal to from_block.',
    })
  }
})

export type GraphFlowFormValues = z.infer<typeof graphFlowFormSchema>

export function graphFlowFormToPayload(values: GraphFlowFormValues) {
  const maxDepth = values.maxDepth.trim() ? Number(values.maxDepth) : 2
  const maxNodes = values.maxNodes.trim() ? Number(values.maxNodes) : 500

  return {
    address: values.address.trim(),
    from_block: values.fromBlock.trim() ? Number(values.fromBlock) : undefined,
    to_block: values.toBlock.trim() ? Number(values.toBlock) : undefined,
    max_depth: Number.isInteger(maxDepth) && maxDepth >= 0 ? maxDepth : 2,
    max_nodes: Number.isInteger(maxNodes) && maxNodes > 0 ? maxNodes : 500,
  }
}
