import { env } from '@/shared/config/env'
import { ApiError } from '@/shared/api/errors'

type ErrorBody = {
  error?: string
}

async function readErrorMessage(response: Response) {
  try {
    const body = (await response.json()) as ErrorBody
    if (body.error) {
      return body.error
    }
  } catch {
    // ignore invalid JSON bodies
  }

  return `Request failed with status ${response.status}`
}

export async function apiRequest<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${env.apiBaseUrl}${path}`, {
    ...init,
    headers: {
      'Content-Type': 'application/json',
      'X-API-Version': '1',
      ...(init?.headers ?? {}),
    },
  })

  if (!response.ok) {
    const message = await readErrorMessage(response)
    throw new ApiError(message, response.status)
  }

  return (await response.json()) as T
}
