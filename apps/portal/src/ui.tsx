export function BrandMark({ compact = false }: { readonly compact?: boolean }) {
  return (
    <span className={compact ? "brand-lockup compact" : "brand-lockup"}>
      <img alt="" aria-hidden="true" src={`${import.meta.env.BASE_URL}favicon.svg`} />
      <span>ffdb</span>
    </span>
  );
}
