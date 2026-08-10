import {
  Archive,
  Bell,
  BookOpenText,
  Check,
  ChartNoAxesCombined,
  Cable,
  ChevronDown,
  ChevronRight,
  ChevronUp,
  CloudUpload,
  Code2,
  CreditCard,
  Database,
  Eye,
  EyeOff,
  ExternalLink,
  House,
  List,
  LockKeyhole,
  Mail,
  Plus,
  RefreshCcw,
  Search,
  Settings,
  ShieldCheck,
  ShoppingCart,
  SquareTerminal,
  Moon,
  Sun,
  Users,
  type LucideIcon,
  type LucideProps,
} from "lucide-react";

export type IconName =
  | "archive"
  | "backup"
  | "bell"
  | "book"
  | "check"
  | "chart"
  | "connect"
  | "chevronDown"
  | "chevronRight"
  | "chevronUp"
  | "code"
  | "creditCard"
  | "database"
  | "external"
  | "eye"
  | "eyeOff"
  | "home"
  | "list"
  | "lock"
  | "mail"
  | "moon"
  | "plus"
  | "search"
  | "settings"
  | "shield"
  | "shoppingCart"
  | "sun"
  | "sync"
  | "terminal"
  | "users";

const icons: Readonly<Record<IconName, LucideIcon>> = {
  archive: Archive,
  backup: CloudUpload,
  bell: Bell,
  book: BookOpenText,
  check: Check,
  chart: ChartNoAxesCombined,
  connect: Cable,
  chevronDown: ChevronDown,
  chevronRight: ChevronRight,
  chevronUp: ChevronUp,
  code: Code2,
  creditCard: CreditCard,
  database: Database,
  external: ExternalLink,
  eye: Eye,
  eyeOff: EyeOff,
  home: House,
  list: List,
  lock: LockKeyhole,
  mail: Mail,
  moon: Moon,
  plus: Plus,
  search: Search,
  settings: Settings,
  shield: ShieldCheck,
  shoppingCart: ShoppingCart,
  sun: Sun,
  sync: RefreshCcw,
  terminal: SquareTerminal,
  users: Users,
};

export function Icon({ name, size = 20, ...props }: {
  readonly name: IconName;
  readonly size?: number;
} & Omit<LucideProps, "size">) {
  const Component = icons[name];
  return <Component aria-hidden="true" size={size} strokeWidth={1.65} {...props} />;
}
