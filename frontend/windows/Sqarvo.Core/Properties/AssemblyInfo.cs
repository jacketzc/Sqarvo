using System.Runtime.CompilerServices;

// 单元测试需要访问 internal 协作类型（例如 QueryHandle 与传输层之间的 IQueryRpc 接口），
// 这样公共 API 不必为了可测性而扩大。
[assembly: InternalsVisibleTo("Sqarvo.Core.Tests")]