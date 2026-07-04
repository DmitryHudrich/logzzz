import { createBrowserRouter } from 'react-router-dom'

import { ErrorBoundary } from '@/app/ui/ErrorBoundary'
import { RouteErrorPage } from '@/app/ui/RouteErrorPage'
import { TransactionGraphPage } from '@/pages/transaction-graph'

export const router = createBrowserRouter([
  {
    path: '/',
    element: (
      <ErrorBoundary>
        <TransactionGraphPage />
      </ErrorBoundary>
    ),
    errorElement: <RouteErrorPage />,
  },
])
