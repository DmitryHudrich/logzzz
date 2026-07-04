import type { PropsWithChildren } from 'react'

import { QueryProvider } from '@/app/providers/query-provider'
import { Toaster } from '@/shared/ui/sonner'
import { TooltipProvider } from '@/shared/ui/tooltip'

export const AppProviders = ({ children }: PropsWithChildren) => {
  return (
    <QueryProvider>
      <TooltipProvider>
        {children}
        <Toaster richColors position='bottom-left' />
      </TooltipProvider>
    </QueryProvider>
  )
}
