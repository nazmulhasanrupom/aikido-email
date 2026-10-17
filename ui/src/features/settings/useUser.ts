import { useQuery } from "@tanstack/react-query";
import { authStateQuery } from "@/api/auth";

/** The logged-in user. Admin pages check `isAdmin`; the API refuses a member as well. */
export function useUser() {
  const { data } = useQuery(authStateQuery);
  const user = data?.user ?? null;
  return {
    user,
    isAdmin: user?.role === "owner" || user?.role === "admin",
    isOwner: user?.role === "owner",
  };
}
