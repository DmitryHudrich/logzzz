import { isRouteErrorResponse, useRouteError } from 'react-router-dom'

import { Button } from '@/shared/ui/button'
import { getErrorMessage } from '@/shared/api'

export const RouteErrorPage = () => {
  const error = useRouteError()

  const message = isRouteErrorResponse(error)
    ? error.statusText || error.data?.toString?.() || 'Route error'
    : getErrorMessage(error, 'Unexpected application error')

  return (
    <div className='flex min-h-screen items-center justify-center bg-background p-6 text-foreground'>
      <div className='w-full max-w-md space-y-4 rounded-xl border border-border bg-card p-6'>
        <h1 className='text-lg font-semibold'>Application error</h1>
        <p className='text-sm text-muted-foreground'>{message}</p>
        <Button type='button' onClick={() => window.location.reload()}>
          Reload page
        </Button>
      </div>
    </div>
  )
}
