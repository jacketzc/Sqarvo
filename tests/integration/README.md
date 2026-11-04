# 集成测试

## 当前状态

尚未实现数据库驱动，因此没有可运行的数据库集成测试。

`docker-compose.yml` 定义了将来驱动集成测试使用的本地数据库。**不要**连接到
真实生产数据库；只能使用这里定义的、带合成数据的测试实例。

## 计划

- MariaDB 11.4 LTS：类型往返（无符号整数、`DECIMAL`、`BIT`、`ENUM`/`SET`、`JSON` 别名、零日期）、
  取消（`KILL QUERY`）、分页游标、catalog = database 的层级语义；
- PostgreSQL 16：类型往返（numeric、timestamptz、uuid、jsonb、数组）、取消、分页游标；
- SQLite：文件数据库、只读模式、无网络依赖的快速回归。

## 启动测试数据库

```bash
cd tests/integration
docker compose up -d
```

## 约定

- 测试数据必须可重复生成（种子脚本），不得依赖人工准备；
- 测试账号只拥有测试库权限；
- 连接参数通过环境变量传入，禁止硬编码密码；
- 清理：测试结束后关闭连接池、删除临时表；容器可保留供反复运行。
