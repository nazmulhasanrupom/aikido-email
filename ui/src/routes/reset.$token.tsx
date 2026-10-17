import { zodResolver } from "@hookform/resolvers/zod";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { useForm } from "react-hook-form";
import { z } from "zod";
import { refreshAuthState, resetPassword } from "@/api/auth";
import { api, unwrap } from "@/api/client";
import { ApiProblem, messageOf } from "@/api/errors";
import { AuthLayout } from "@/components/AuthLayout";
import { Button } from "@/components/Button";
import { PasswordField } from "@/components/PasswordField";
import { StrengthMeter } from "@/components/StrengthMeter";
import { HOME } from "@/lib/next";

export const Route = createFileRoute("/reset/$token")({ component: ResetPage });

const schema = z.object({
  new_password: z
    .string()
    .min(12, "Use at least 12 characters.")
    .max(128, "Use at most 128 characters."),
});
type Values = z.infer<typeof schema>;

function ResetPage() {
  const { token } = Route.useParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [problem, setProblem] = useState<string>();
  const info = useQuery({
    queryKey: ["reset", token],
    queryFn: () => unwrap(api.GET("/v1/auth/reset/{token}", { params: { path: { token } } })),
    retry: false,
  });
  const {
    register,
    handleSubmit,
    setError,
    watch,
    formState: { errors, isSubmitting },
  } = useForm<Values>({ resolver: zodResolver(schema) });
  const password = watch("new_password");

  if (info.isPending) return <AuthLayout title="Set a new password">{null}</AuthLayout>;
  if (info.isError) {
    return (
      <AuthLayout title="This link does not work">
        <p className="text-muted">{messageOf(info.error)}</p>
      </AuthLayout>
    );
  }

  async function onSubmit(values: Values) {
    setProblem(undefined);
    try {
      await resetPassword(token, values.new_password);
      await refreshAuthState(queryClient);
      await navigate({ href: HOME });
    } catch (e) {
      if (e instanceof ApiProblem && e.code.startsWith("password_"))
        setError("new_password", { message: messageOf(e) });
      else setProblem(messageOf(e));
    }
  }

  return (
    <AuthLayout title="Set a new password" intro={info.data.email}>
      <form onSubmit={handleSubmit(onSubmit)} noValidate className="flex flex-col gap-4">
        <div className="flex flex-col gap-2">
          <PasswordField
            label="New password"
            autoComplete="new-password"
            autoFocus
            help="At least 12 characters. You are logged out everywhere else."
            error={errors.new_password?.message}
            {...register("new_password")}
          />
          <StrengthMeter password={password ?? ""} inputs={[info.data.email]} />
        </div>
        {problem && (
          <p role="alert" className="text-sm text-danger">
            {problem}
          </p>
        )}
        <Button type="submit" busy={isSubmitting}>
          Save password
        </Button>
      </form>
    </AuthLayout>
  );
}
