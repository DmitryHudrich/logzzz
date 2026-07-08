import { apiRequest, ApiError } from '@/shared/api'
import { parseLabelResponse } from '@/entities/label/model/label-schema'
import type { AddressLabel, UpsertAddressLabelPayload } from '@/entities/label/model/label'

export async function fetchAddressLabel(address: string, chainId = 1): Promise<AddressLabel | null> {
  try {
    const response = await apiRequest<unknown>(`/labels/${address}?chain_id=${chainId}`)
    return mapAddressLabel(parseLabelResponse(response))
  } catch (error) {
    if (error instanceof ApiError && error.status === 404) {
      return null
    }
    throw error
  }
}

export async function upsertAddressLabel(payload: UpsertAddressLabelPayload): Promise<AddressLabel> {
  const response = await apiRequest<unknown>('/labels', {
    method: 'POST',
    body: JSON.stringify({
      address: payload.address,
      category: payload.category,
      chain_id: payload.chainId ?? 1,
      label_name: payload.labelName ?? null,
      label_source: payload.labelSource ?? 'manual',
      label_url: payload.labelUrl ?? null,
      risk_score: payload.riskScore ?? null,
      sanction_list: payload.sanctionList ?? null,
    }),
  })
  return mapAddressLabel(parseLabelResponse(response))
}

export async function deleteAddressLabel(address: string, chainId = 1): Promise<void> {
  await apiRequest<void>(`/labels/${address}?chain_id=${chainId}`, {
    method: 'DELETE',
  })
}

function mapAddressLabel(dto: ReturnType<typeof parseLabelResponse>): AddressLabel {
  return {
    entityId: dto.entity_id,
    addresses: dto.addresses,
    category: dto.category,
    labelName: dto.label_name ?? null,
    labelSource: dto.label_source ?? null,
    labelUrl: dto.label_url ?? null,
    sanctionList: dto.sanction_list ?? null,
    riskScore: dto.risk_score,
  }
}
