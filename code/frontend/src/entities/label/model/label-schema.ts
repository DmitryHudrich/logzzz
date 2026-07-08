import { z } from 'zod'

export const labelResponseSchema = z.object({
  entity_id: z.string(),
  addresses: z.array(z.string()),
  category: z.string(),
  label_name: z.string().nullable().optional(),
  label_source: z.string().nullable().optional(),
  label_url: z.string().nullable().optional(),
  risk_score: z.number(),
  sanction_list: z.string().nullable().optional(),
})

export const labelRequestSchema = z.object({
  address: z.string(),
  category: z.string(),
  chain_id: z.number().int().nonnegative().optional(),
  label_name: z.string().nullable().optional(),
  label_source: z.string().nullable().optional(),
  label_url: z.string().nullable().optional(),
  risk_score: z.number().int().nonnegative().nullable().optional(),
  sanction_list: z.string().nullable().optional(),
})

export type LabelResponseDto = z.infer<typeof labelResponseSchema>
export type LabelRequestDto = z.infer<typeof labelRequestSchema>

export function parseLabelResponse(data: unknown) {
  return labelResponseSchema.parse(data)
}
