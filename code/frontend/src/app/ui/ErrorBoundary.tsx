import { Component, type ErrorInfo, type PropsWithChildren, type ReactNode } from 'react'

import { Button } from '@/shared/ui/button'
import { getErrorMessage } from '@/shared/api'

type ErrorBoundaryProps = PropsWithChildren<{
  fallback?: ReactNode
}>

type ErrorBoundaryState = {
  error: Error | null
}

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null }

  static getDerivedStateFromError(error: Error) {
    return { error }
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error('Unhandled UI error:', error, info.componentStack)
  }

  private reset = () => {
    this.setState({ error: null })
  }

  render() {
    if (this.state.error) {
      if (this.props.fallback) {
        return this.props.fallback
      }

      return (
        <div className='flex min-h-screen items-center justify-center bg-background p-6 text-foreground'>
          <div className='w-full max-w-md space-y-4 rounded-xl border border-border bg-card p-6'>
            <h1 className='text-lg font-semibold'>Something went wrong</h1>
            <p className='text-sm text-muted-foreground'>
              {getErrorMessage(this.state.error, 'An unexpected error occurred.')}
            </p>
            <Button type='button' onClick={this.reset}>
              Try again
            </Button>
          </div>
        </div>
      )
    }

    return this.props.children
  }
}
