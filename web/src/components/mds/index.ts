/**
 * MDS — Multica-derived Design System primitives barrel.
 * See commercial/docs/multica-redesign-spec.md §4 for component specs.
 */
export { Button, buttonVariants, type ButtonProps } from './button';
export {
  Card,
  CardHeader,
  CardTitle,
  CardDescription,
  CardAction,
  CardContent,
  CardFooter,
} from './card';
export { Badge, badgeVariants, type BadgeProps } from './badge';
export { Input } from './input';
export { Textarea } from './textarea';
export {
  Select,
  SelectGroup,
  SelectValue,
  SelectTrigger,
  SelectContent,
  SelectItem,
  SelectLabel,
  SelectSeparator,
} from './select';
export {
  Dialog,
  DialogTrigger,
  DialogClose,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from './dialog';
export {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuGroup,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuShortcut,
} from './dropdown-menu';
export { Tabs, TabsList, TabsTab, TabsPanel } from './tabs';
export { Segmented, type SegmentedOption } from './segmented';
export {
  Table,
  TableHeader,
  TableBody,
  TableFooter,
  TableRow,
  TableHead,
  TableCell,
  TableCaption,
} from './table';
export {
  Tooltip,
  TooltipProvider,
  TooltipTrigger,
  TooltipContent,
} from './tooltip';
export {
  Popover,
  PopoverTrigger,
  PopoverClose,
  PopoverContent,
} from './popover';
export { Switch } from './switch';
export { Checkbox } from './checkbox';
export { Separator } from './separator';
export { Skeleton } from './skeleton';
export {
  Sheet,
  SheetTrigger,
  SheetClose,
  SheetContent,
  SheetHeader,
  SheetTitle,
  SheetDescription,
  SheetFooter,
} from './sheet';
export { Empty } from './empty';
export { Spinner } from './spinner';
export { SubmitButton, type SubmitButtonState } from './submit-button';

// ── Layout layer (WP0.3) ──────────────────────────────────────────────
export {
  SidebarProvider,
  Sidebar,
  SidebarRail,
  SidebarHeader,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupLabel,
  SidebarMenu,
  SidebarMenuItem,
  SidebarMenuButton,
  sidebarMenuButtonVariants,
  SidebarMenuBadge,
  SidebarTrigger,
  SidebarInset,
  useSidebar,
  useIsMobile,
} from './sidebar';
export { PageHeader } from './page-header';
export {
  CollectionPageHeader,
  CollectionPageState,
  toCompactAction,
} from './collection-page';
export {
  BreadcrumbHeader,
  type BreadcrumbSegment,
} from './breadcrumb-header';
export {
  SettingsShell,
  SettingsTab,
  SettingsSection,
  SettingsCard,
  SettingsRow,
  SettingsSaveState,
  type SettingsNavItem,
  type SettingsNavGroup,
  type SettingsRowTier,
  type SettingsSaveStatus,
} from './settings-layout';
export {
  ResizablePanelGroup,
  ResizablePanel,
  ResizableHandle,
} from './resizable';
export {
  ListGridContainer,
  ListGridHeader,
  ListGridHeaderCell,
  ListGridRow,
  ListGridCell,
  useRowLink,
  type ListGridVirtualConfig,
} from './list-grid';
export {
  ActorAvatar,
  type ActorType,
  type ActorStatus,
  type ActorAvatarSize,
} from './actor-avatar';
export { NavProgress } from './nav-progress';
