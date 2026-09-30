import * as React from "react"

// The shell's narrow layout starts below 760 px (src/app/layout.ts MEDIUM_MIN),
// and index.css moves Tailwind's `md` there too; shadcn's default 768 would
// turn the icon rail into an off-screen sheet between 760 and 767 px.
const MOBILE_BREAKPOINT = 760

export function useIsMobile() {
  const [isMobile, setIsMobile] = React.useState<boolean | undefined>(undefined)

  React.useEffect(() => {
    const mql = window.matchMedia(`(max-width: ${MOBILE_BREAKPOINT - 1}px)`)
    const onChange = () => {
      setIsMobile(window.innerWidth < MOBILE_BREAKPOINT)
    }
    mql.addEventListener("change", onChange)
    setIsMobile(window.innerWidth < MOBILE_BREAKPOINT)
    return () => mql.removeEventListener("change", onChange)
  }, [])

  return !!isMobile
}
