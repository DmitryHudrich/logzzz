export const ENTITY_CATEGORIES = [
  'exchange',
  'mixer',
  'bridge',
  'defi',
  'scam',
  'gambling',
  'darknet',
  'mining',
  'unknown',
  'sanctioned',
] as const

export type EntityCategory = (typeof ENTITY_CATEGORIES)[number]

export const SANCTION_LISTS = ['ofac', 'eu', 'un', 'other'] as const
export type SanctionList = (typeof SANCTION_LISTS)[number]

export type AddressLabel = {
  entityId: string
  category: EntityCategory | string
  labelName: string | null
  labelSource: string | null
  labelUrl: string | null
  sanctionList: string | null
  riskScore: number
  addresses: string[]
}

export type UpsertAddressLabelPayload = {
  address: string
  category: EntityCategory | string
  labelName?: string | null
  labelSource?: string | null
  labelUrl?: string | null
  sanctionList?: SanctionList | string | null
  riskScore?: number | null
  chainId?: number
}
